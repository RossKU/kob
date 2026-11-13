//! Submission, mempool conflicts, acceptance tracking and rollback (`docs/spec/matcher.md` §1.3,
//! §7, §8).
//!
//! Every transaction is engine-validated before it reaches [`Tracker::submit`].
//!
//! # Conflicts
//!
//! - `RejectDoubleSpendInMempool` is benign. A competing matcher, a cancel or a refund took the
//!   input. The order whose input the node names backs off exponentially. When the input belongs to
//!   no order ([`Tracked::owners`]), every order of the transaction backs off.
//! - Nothing is replaced blindly. The matcher never calls `submitTransactionReplacement`. The
//!   runner replaces only its own stuck keeper kills and refunds
//!   (`crate::matcher::run::Runner::replace_stuck`).
//! - A missing input is an orphan: its spend is already in a block the book has not shown yet. The
//!   refusal names no outpoint, so the tracker asks the node which order inputs are still unspent
//!   ([`Tracked::addresses`]). The orders whose inputs are gone back off, and the other orders of
//!   the transaction are planned again at once. Every order backs off only when the node cannot
//!   tell.
//! - A mempool double spend of one of the operator's own funding coins backs off no order. The
//!   spender is a transaction this process does not know: one from before a restart that the
//!   journal missed, a withdraw, or a second process on the same key. The coin is held out of the
//!   funding for `hold_daa`, or until the node's UTXO set no longer lists it ([`Tracker::held`]).
//!   The batch is planned again at once over the other coins.
//!
//! # Acceptance
//!
//! - Acceptance comes from `getVirtualChainFromBlockV2` (`Low`). A transaction is accepted when a
//!   chain block accepts it and final (done) `final_depth` DAA later.
//! - A `removed` chain block moves its transactions back to pending (reorg), where they are
//!   re-checked.
//! - While a transaction is pending or accepted but not final, the outpoints it spends are excluded
//!   from planning, so no order is planned twice.
//!
//! # Pending transactions
//!
//! A pending transaction that no chain block accepted within `pending_timeout` DAA is not
//! forgotten. It is resent to the node, which settles it ([`Tracker::recheck_pending`]).
//!
//! - "Already in the mempool", or taken again: it stays pending with its inputs reserved, and is
//!   checked again after another `pending_timeout`. A node keeps a transaction until it is mined or
//!   evicted, so forgetting it would make its own funding coin refuse every later batch as a
//!   mempool double spend.
//! - Refused because its inputs went elsewhere: it is dropped (logged, `dropped_total`) and its
//!   orders back off.
//!
//! A pending transaction is also dropped at once when the book no longer lists one of its order
//! inputs on two steps in a row and the node confirms that input is spent. Another transaction won
//! the race. The order whose input is gone backs off and the others are planned again
//! ([`Tracker::drop_lost`]).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::book::{CovId, Outpoint};
use super::node::{classify_submit_error, ChainUpdate, NodeApi, RpcError, SubmitOutcome};

/// Tracker settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackerConfig {
    /// DAA after acceptance at which a transaction is final (default 100, §1.3).
    pub final_depth: u64,
    /// DAA after which an unaccepted transaction is resent to the node as a check. The check
    /// repeats every `pending_timeout` DAA while the node holds it. Default 600, a minute
    /// ([`Tracker::recheck_pending`]).
    pub pending_timeout: u64,
    /// Age in DAA past which a submission without a verdict is retired, not sent again
    /// ([`Tracker::retire`]). Such a submission came back from the in-flight journal after a
    /// restart, or its submit got no answer. Default 6000, ten times `pending_timeout`.
    pub max_resend_age: u64,
    /// First back-off of an order after a conflict (DAA; doubles per attempt).
    pub backoff_base: u64,
    /// Longest back-off (DAA).
    pub backoff_max: u64,
    /// DAA a funding coin of the operator stays out of the funding after a mempool transaction
    /// unknown to the tracker spends it. The hold ends earlier when the node's UTXO set no longer
    /// lists the coin. Default 3000, five minutes.
    pub hold_daa: u64,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        TrackerConfig {
            final_depth: 100,
            pending_timeout: 600,
            max_resend_age: 6_000,
            backoff_base: 20,
            backoff_max: 600,
            hold_daa: 3_000,
        }
    }
}

/// Whether a node's refusal says the transaction is already accepted by the chain.
///
/// That is rusty-kaspa's `RejectAlreadyAccepted`. A block this process may not have followed
/// (before a restart) accepted the transaction.
pub fn accepted_by_consensus(msg: &str) -> bool {
    msg.to_ascii_lowercase().contains("already accepted by the consensus")
}

/// What [`Tracker::recheck_pending`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recheck {
    /// Pending transactions the node still holds (or took again): kept.
    pub kept: Vec<[u8; 32]>,
    /// Of `kept`, those the node did not hold any more and took again (re-broadcast).
    pub rebroadcast: Vec<[u8; 32]>,
    /// Transactions the node refused, with its answer: dropped, and their orders back off.
    pub dropped: Vec<(Tracked, String)>,
    /// Transactions the node reports already accepted by the chain: done.
    pub accepted_earlier: Vec<Tracked>,
    /// Transactions not sent again because they would no longer be valid ([`Tracker::stale`]).
    pub retired: Vec<[u8; 32]>,
}

/// A transaction the tracker follows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tracked {
    /// Id of the transaction.
    pub txid: [u8; 32],
    /// Outpoints the transaction spends.
    pub spends: BTreeSet<Outpoint>,
    /// Order covenant ids it spends (legs, merged entries).
    pub orders: BTreeSet<CovId>,
    /// The order each input belongs to, where known (an order's own UTXO, its custody).
    ///
    /// A mempool double spend of one of these outpoints backs off that order only. A conflict on
    /// any other input, or an empty map, backs off every order of the transaction.
    pub owners: BTreeMap<Outpoint, CovId>,
    /// The address of each input `owners` names, where known. On a missing input the node is asked
    /// which of these are still unspent.
    pub addresses: BTreeMap<Outpoint, String>,
    /// The outpoints of this transaction that its children spend (chained steps).
    pub parent: Option<[u8; 32]>,
    /// DAA score at which the transaction was submitted.
    pub submitted_daa: u64,
    /// `RpcTransaction` JSON, for an idempotent resend after a reorg.
    pub rpc_tx: Value,
    /// Accepting chain block and its DAA score.
    pub accepted: Option<(String, u64)>,
    /// What the transaction is (for logs and metrics): `match`, `refund`, `kill`, `close`, `sweep`.
    pub kind: String,
    /// Operator profit promised by the build (sompi).
    pub profit: i64,
    /// Network fee paid (sompi).
    pub fee: u64,
    /// Fee rate (sompi per gram, `crate::fee`).
    pub fee_rate: u64,
    /// The submit call got no verdict (transport error or timeout), so the node may hold the
    /// transaction. Cleared by the node's answer to a resend or by the transaction's acceptance.
    pub unconfirmed: bool,
    /// The earliest UTC deadline (unix seconds) of the day orders the transaction fills.
    ///
    /// The transaction is never sent again from the deadline on, nor within the clock margin before
    /// it (`docs/spec/matcher.md` §2.3: no fill after the deadline; [`Tracker::stale`]).
    pub deadline: Option<u64>,
    /// The DAA score from which the transaction is never sent again.
    ///
    /// It is the soft expiry of the first order the transaction fills, or the end of a maintenance
    /// sale's freshness (the sale's price was the book's when it was planned).
    pub resend_until: Option<u64>,
    /// Set when the transaction is no longer sent ([`Tracker::retire`]): the DAA score until which
    /// its inputs stay reserved.
    ///
    /// A mempool may still hold the transaction. The chain then shows it and it is followed to the
    /// end like any other. After that score it is forgotten ([`Tracker::forget_retired`]).
    pub retired: Option<u64>,
}

/// What a resend is checked against ([`Tracker::stale`]).
///
/// It holds the virtual DAA score, the local UTC time (unix seconds) and the clock margin a day
/// order's deadline keeps (the clock check's bound plus the measured skew, `matcher::run`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResendClock {
    /// Virtual DAA score.
    pub daa: u64,
    /// Local UTC time, unix seconds.
    pub utc: u64,
    /// Seconds a day order's deadline keeps clear of the local clock.
    pub margin_secs: u64,
}

impl ResendClock {
    /// At `daa`, with no deadline in view (tests, and callers without a clock).
    pub fn at(daa: u64) -> ResendClock {
        ResendClock { daa, utc: 0, margin_secs: 0 }
    }
}

/// Events of one tracker step (for logs and metrics).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackerEvents {
    /// Transactions a chain block accepted in this step.
    pub accepted: Vec<[u8; 32]>,
    /// Transactions that became final.
    pub finalized: Vec<Tracked>,
    /// Transactions whose accepting block was reorged away.
    pub rolled_back: Vec<[u8; 32]>,
}

/// The outpoint a node's double-spend rejection names.
///
/// The message is rusty-kaspa's `output (<txid>, <index>) already spent by transaction <txid> in
/// the mempool`, where `TransactionOutpoint` displays as `(<txid>, <index>)`. The form `output
/// <txid>:<index> ...` is read too.
pub fn double_spent_outpoint(msg: &str) -> Option<Outpoint> {
    let at = msg.find("output ")? + "output ".len();
    let rest = &msg[at..];
    let end = rest.find(" already spent by transaction")?;
    let s = rest[..end].trim();
    let (txid, index) = match s.strip_prefix('(').and_then(|x| x.strip_suffix(')')) {
        Some(inner) => inner.split_once(',')?,
        None => s.rsplit_once(':')?,
    };
    let txid = crate::hex::Hash32::parse(txid.trim()).ok()?.0;
    Some((txid, index.trim().parse().ok()?))
}

/// The orders a lost race backs off.
///
/// They are the order whose input a double spend names. For a missing input they are the orders
/// whose inputs the node no longer holds. Otherwise they are every order of the transaction.
async fn lost_orders<N: NodeApi>(node: &N, t: &Tracked, outcome: SubmitOutcome, msg: Option<&str>) -> BTreeSet<CovId> {
    if outcome == SubmitOutcome::MissingInput {
        if let Some(ids) = missing_owners(node, t).await {
            return ids;
        }
    }
    conflict_orders(t, outcome, msg)
}

/// The orders a refused submission backs off.
///
/// On a mempool double spend of an input whose order is known ([`Tracked::owners`]), that order
/// alone backs off, and the others are planned again at once. Otherwise every order of the
/// transaction backs off.
fn conflict_orders(t: &Tracked, outcome: SubmitOutcome, msg: Option<&str>) -> BTreeSet<CovId> {
    if outcome == SubmitOutcome::DoubleSpend {
        if let Some(id) = msg.and_then(double_spent_outpoint).and_then(|op| t.owners.get(&op)) {
            return [*id].into_iter().collect();
        }
    }
    t.orders.clone()
}

/// The orders a node's generic refusal ([`SubmitOutcome::Rejected`]) backs off.
///
/// The refusal names no order, so the batch is searched by halves across ticks. Half of the orders
/// sit out. The other half is planned again at once and, if the node refuses it again, loses half
/// of itself the same way, until a single order is backed off. One order that makes a node-only
/// policy refuse every batch it is in therefore costs the others at most one backoff each, never
/// the whole batch every time.
///
/// The half is chosen by a hash of the order id and the refusal count, keyed by a key that is
/// random per process. A fixed choice such as the lower half by id would make an order with a
/// ground low covenant id (cheap to make) the permanent victim of the lowest honest ids of every
/// batch it is in.
fn rejected_orders(salt: &std::collections::hash_map::RandomState, round: u64, orders: &BTreeSet<CovId>) -> BTreeSet<CovId> {
    use std::hash::BuildHasher;
    let half = orders.len().div_ceil(2);
    let mut keyed: Vec<(u64, CovId)> = orders.iter().map(|id| (salt.hash_one((id, round)), *id)).collect();
    keyed.sort_unstable();
    keyed.into_iter().take(half).map(|(_, id)| id).collect()
}

/// The orders of `t` whose inputs the node no longer holds, asked by address after a missing-input
/// refusal.
///
/// `None` when the node cannot tell:
///
/// - no address is known;
/// - the node has no UTXO index (`--utxoindex`), so an address query proves nothing and nothing is
///   taken as spent;
/// - the query failed;
/// - every order input is still there, so the missing input is another one.
async fn missing_owners<N: NodeApi>(node: &N, t: &Tracked) -> Option<BTreeSet<CovId>> {
    let addrs: BTreeSet<&String> = t.owners.keys().filter_map(|op| t.addresses.get(op)).collect();
    if addrs.is_empty() {
        return None;
    }
    if !node.server_info().await.is_ok_and(|i| i.has_utxo_index) {
        return None;
    }
    let addrs: Vec<String> = addrs.into_iter().cloned().collect();
    let utxos = node.utxos_by_addresses(&addrs).await.ok()?;
    let live: BTreeSet<Outpoint> = utxos.iter().map(|u| (u.transaction_id, u.index)).collect();
    let gone: BTreeSet<CovId> =
        t.owners.iter().filter(|(op, _)| t.addresses.contains_key(*op) && !live.contains(*op)).map(|(_, id)| *id).collect();
    (!gone.is_empty()).then_some(gone)
}

/// Follows submitted transactions until they are final.
#[derive(Debug, Default)]
pub struct Tracker {
    /// Settings.
    pub cfg: TrackerConfig,
    /// Last chain block seen (VSPC cursor).
    pub cursor: Option<String>,
    /// The followed transactions by id.
    pub txs: BTreeMap<[u8; 32], Tracked>,
    /// Order id -> (back off until DAA, attempts).
    pub backoff: BTreeMap<CovId, (u64, u32)>,
    /// The key of the hash that picks the half of a refused batch to back off
    /// ([`rejected_orders`]). Random per process.
    half_salt: std::collections::hash_map::RandomState,
    /// Submissions the node took.
    pub submitted: u64,
    /// Submissions refused for a conflict: a mempool double spend, a missing input or a lost race.
    pub conflicts: u64,
    /// Submissions the node refused with a generic refusal ([`SubmitOutcome::Rejected`]).
    pub rejected: u64,
    /// Submissions without a verdict.
    pub unknown: u64,
    /// Transactions that became final.
    pub finalized_count: u64,
    /// Sum of the profit (sompi) of the final transactions.
    pub finalized_profit: i64,
    /// Pending transactions that had an order input missing from the last book
    /// ([`Tracker::drop_lost`]).
    pub lost: BTreeSet<[u8; 32]>,
    /// The operator's funding coins (its P2PK outpoints) at the last funding read
    /// ([`Tracker::note_operator_coins`]). A mempool double spend naming one of them holds the coin
    /// instead of backing off the batch's orders.
    pub operator_coins: BTreeSet<Outpoint>,
    /// Funding coins that a mempool transaction unknown to the tracker spends, with the DAA score
    /// until which they stay out of the funding (`TrackerConfig::hold_daa`). A coin is dropped
    /// earlier once the node's UTXO set no longer lists it.
    pub held: BTreeMap<Outpoint, u64>,
    /// DAA score at which each pending transaction was last checked with the node.
    ///
    /// See [`Tracker::recheck_pending`].
    pub checked: BTreeMap<[u8; 32], u64>,
    /// Transactions given up: refused by the node at a recheck or a resend, or a lost race
    /// ([`Tracker::drop_lost`]).
    pub dropped_total: u64,
    /// Funding coins held after a mempool double spend ([`Tracker::held`]).
    pub held_total: u64,
    /// Unix time (seconds) of the last submission the node took (0: none yet).
    pub last_submit_unix: u64,
    /// Transactions not sent again because they would no longer be valid ([`Tracker::stale`]): a
    /// day order's deadline, an order's soft expiry, a maintenance sale's stale price, or a journal
    /// entry too old to send blindly.
    pub resend_dropped_total: u64,
    /// Submissions without a verdict that the node refused for a missing input at a resend, so they
    /// were possibly mined while this process was down.
    ///
    /// They are retired, not dropped. The store shows them accepted once it has caught up, or they
    /// count as dropped when forgotten ([`Tracker::forget_retired`]).
    pub doubtful: BTreeSet<[u8; 32]>,
    /// Transactions the in-flight journal brought back at start, until the node's first answer
    /// about each.
    pub restored: BTreeSet<[u8; 32]>,
    /// Retired transactions the node's mempool did not hold at the last check: the DAA score of
    /// that check and the number of misses in a row ([`Tracker::retired_to_check`]).
    pub retired_misses: BTreeMap<[u8; 32], (u64, u32)>,
}

/// A retired transaction's mempool is checked at most this often (DAA, about ten seconds).
pub const RETIRED_CHECK_DAA: u64 = 100;
/// Misses in a row after which a retired transaction is forgotten before its hold ends.
///
/// A transaction mined between two checks leaves the mempool too, so the chain shows it accepted
/// (and it is followed as any other) before a second miss.
pub const RETIRED_MISSES: u32 = 2;

impl Tracker {
    /// A tracker with no transactions.
    pub fn new(cfg: TrackerConfig) -> Self {
        Tracker { cfg, ..Default::default() }
    }

    /// Outpoints no plan may spend: those of pending and accepted but not final transactions, and
    /// held funding coins.
    pub fn spent_outpoints(&self) -> BTreeSet<Outpoint> {
        let mut s: BTreeSet<Outpoint> = self.txs.values().flat_map(|t| t.spends.iter().copied()).collect();
        s.extend(self.held.keys().copied());
        s
    }

    /// The operator's funding coins as the node lists them now. A held coin the node no longer
    /// lists (its spender was mined, or it is gone) and one whose hold ran out are released.
    pub fn note_operator_coins(&mut self, coins: BTreeSet<Outpoint>, daa: u64) {
        self.held.retain(|op, until| *until > daa && coins.contains(op));
        self.operator_coins = coins;
    }

    /// Order ids backed off at `daa`.
    pub fn backed_off(&self, daa: u64) -> BTreeSet<CovId> {
        self.backoff.iter().filter(|(_, (until, _))| *until > daa).map(|(k, _)| *k).collect()
    }

    /// Forgets the backoffs that ran out longer than `backoff_max` ago, since their attempt count
    /// no longer doubles anything. The map stays as small as the set of orders that recently lost a
    /// race.
    pub fn prune_backoff(&mut self, daa: u64) {
        let keep = self.cfg.backoff_max;
        self.backoff.retain(|_, (until, _)| until.saturating_add(keep) > daa);
    }

    /// DAA the oldest pending (unaccepted) transaction has waited at `daa` (0: none pending). A
    /// retired transaction is not waited for.
    pub fn oldest_pending_daa(&self, daa: u64) -> u64 {
        self.txs
            .values()
            .filter(|t| t.accepted.is_none() && t.retired.is_none())
            .map(|t| daa.saturating_sub(t.submitted_daa))
            .max()
            .unwrap_or(0)
    }

    /// Why `t` must not be sent again now (`None`: it may be sent).
    ///
    /// The reasons are:
    ///
    /// - a day order it fills reached its deadline, or is within the clock margin of it;
    /// - an order it fills reached its soft expiry, or a maintenance sale's price is no longer
    ///   current ([`Tracked::resend_until`]);
    /// - a submission without a verdict is older than `max_resend_age`. A journal entry from before
    ///   a long downtime may have been planned on facts that changed.
    ///
    /// The planning gates see only new plans, so a resend is checked here.
    pub fn stale(&self, t: &Tracked, c: &ResendClock) -> Option<String> {
        if let Some(d) = t.deadline.filter(|d| c.utc.saturating_add(c.margin_secs) >= *d) {
            return Some(format!(
                "a day order it fills has reached its deadline (UTC {d}, the local clock {} with a margin of {} s)",
                c.utc, c.margin_secs
            ));
        }
        if let Some(b) = t.resend_until.filter(|b| c.daa >= *b) {
            return Some(format!("an order it fills expired, or the sale's price is no longer current (from DAA {b})"));
        }
        let age = c.daa.saturating_sub(t.submitted_daa);
        if t.unconfirmed && age > self.cfg.max_resend_age {
            return Some(format!("it was submitted {age} DAA ago without a verdict (before a restart): not sent again blindly"));
        }
        None
    }

    /// Stops sending `id` (`why`): logged and counted ([`Tracker::resend_dropped_total`]).
    ///
    /// Its inputs stay reserved for `hold_daa`, since a mempool may still hold it, and its
    /// acceptance is followed meanwhile. Then it is forgotten ([`Tracker::forget_retired`]), or
    /// earlier once the nodes say twice in a row that their mempools do not hold it
    /// ([`Tracker::retired_checked`]).
    pub fn retire(&mut self, id: &[u8; 32], why: &str, daa: u64) {
        let hold = self.cfg.hold_daa;
        if !self.hold_retired(id, daa) {
            return;
        }
        self.resend_dropped_total += 1;
        let Some(t) = self.txs.get(id) else { return };
        tracing::warn!(
            txid = %kob_protocol::json::to_hex(id),
            kind = %t.kind,
            reason = why,
            hold_daa = hold,
            "a pending transaction is not sent again (it would no longer be valid); its inputs stay reserved meanwhile"
        );
    }

    /// Marks `id` retired until `hold_daa` from `daa`. Returns `false` when it is not tracked or
    /// already retired.
    fn hold_retired(&mut self, id: &[u8; 32], daa: u64) -> bool {
        let hold = self.cfg.hold_daa;
        let Some(t) = self.txs.get_mut(id).filter(|t| t.retired.is_none()) else { return false };
        t.retired = Some(daa.saturating_add(hold));
        t.unconfirmed = false;
        self.checked.remove(id);
        true
    }

    /// The retired transactions no chain block accepted by `daa` ([`Tracked::retired`]): forgotten,
    /// so their inputs are free again.
    ///
    /// Call it once the store has caught up, since a transaction mined while this process was down
    /// shows as accepted first. A doubtful one ([`Tracker::doubtful`]) counts as dropped then.
    pub fn forget_retired(&mut self, daa: u64) -> Vec<Tracked> {
        let gone: Vec<[u8; 32]> =
            self.txs.values().filter(|t| t.accepted.is_none() && t.retired.is_some_and(|r| daa >= r)).map(|t| t.txid).collect();
        let mut out = vec![];
        for id in gone {
            if let Some(t) = self.txs.remove(&id) {
                self.checked.remove(&id);
                if self.doubtful.remove(&id) {
                    self.dropped_total += 1;
                }
                out.push(t);
            }
        }
        let txs = &self.txs;
        self.doubtful.retain(|id| txs.contains_key(id));
        self.retired_misses.retain(|id, _| txs.contains_key(id));
        out
    }

    /// The retired transactions to ask the node's mempool about at `daa`.
    ///
    /// They are unaccepted, their hold is not over, and they were not asked within
    /// [`RETIRED_CHECK_DAA`].
    pub fn retired_to_check(&self, daa: u64) -> Vec<[u8; 32]> {
        self.txs
            .values()
            .filter(|t| t.accepted.is_none() && t.retired.is_some_and(|r| daa < r))
            .filter(|t| self.retired_misses.get(&t.txid).is_none_or(|(at, _)| daa >= at.saturating_add(RETIRED_CHECK_DAA)))
            .map(|t| t.txid)
            .collect()
    }

    /// Records the node's answer about retired `id` at `daa`.
    ///
    /// `Some(false)` means the node does not hold it, `Some(true)` that it does, and `None` that it
    /// is unsure (the hold stays). At [`RETIRED_MISSES`] misses in a row the transaction is
    /// forgotten before its hold ends and returned, and its inputs are free again. A doubtful one
    /// counts as dropped then, as [`Tracker::forget_retired`] does at the hold's end.
    pub fn retired_checked(&mut self, id: &[u8; 32], held: Option<bool>, daa: u64) -> Option<Tracked> {
        if !self.txs.get(id).is_some_and(|t| t.accepted.is_none() && t.retired.is_some()) {
            self.retired_misses.remove(id);
            return None;
        }
        let misses = match held {
            Some(false) => self.retired_misses.get(id).map_or(0, |m| m.1) + 1,
            // Held or unsure: no miss in a row.
            _ => 0,
        };
        if misses < RETIRED_MISSES {
            self.retired_misses.insert(*id, (daa, misses));
            return None;
        }
        self.retired_misses.remove(id);
        self.checked.remove(id);
        if self.doubtful.remove(id) {
            self.dropped_total += 1;
        }
        self.txs.remove(id)
    }

    /// A mempool double spend of `t` that names one of the operator's funding coins (`msg`): the
    /// coin is held ([`Tracker::held`]) and no order backs off. Returns `false` when the double
    /// spend is about something else.
    fn hold_funding(&mut self, t: &Tracked, msg: Option<&str>, daa: u64) -> bool {
        let Some(op) = msg.and_then(double_spent_outpoint) else { return false };
        if !self.operator_coins.contains(&op) || t.owners.contains_key(&op) || !t.spends.contains(&op) {
            return false;
        }
        self.held.insert(op, daa.saturating_add(self.cfg.hold_daa));
        self.held_total += 1;
        tracing::warn!(
            coin = %format!("{}:{}", kob_protocol::json::to_hex(&op.0), op.1),
            hold_daa = self.cfg.hold_daa,
            "a funding coin of the operator is spent by a mempool transaction this process does not track; it is held out of the funding (no order backs off)"
        );
        true
    }

    fn submitted_now(&mut self) {
        self.last_submit_unix = super::book::utc_now();
    }

    fn back_off(&mut self, ids: &BTreeSet<CovId>, daa: u64) {
        for id in ids {
            let e = self.backoff.entry(*id).or_insert((0, 0));
            e.1 = e.1.saturating_add(1);
            let d = self.cfg.backoff_base.saturating_mul(1u64 << (e.1 - 1).min(16)).min(self.cfg.backoff_max);
            e.0 = daa + d;
        }
    }

    /// Submits a transaction and records it. Returns the node's verdict.
    pub async fn submit<N: NodeApi>(&mut self, node: &N, t: Tracked, daa: u64) -> SubmitOutcome {
        if let Some(p) = t.parent {
            if !self.txs.contains_key(&p) {
                // A chained step whose parent failed is never submitted.
                return SubmitOutcome::MissingInput;
            }
        }
        let mut msg: Option<String> = None;
        let outcome = match node.submit(t.rpc_tx.clone()).await {
            Ok(_) => SubmitOutcome::Accepted,
            Err(RpcError::Node(m)) => {
                let o = classify_submit_error(&m);
                msg = Some(m);
                o
            }
            Err(_) => SubmitOutcome::Unknown,
        };
        match outcome {
            SubmitOutcome::Accepted | SubmitOutcome::AlreadyKnown => {
                self.submitted += 1;
                self.submitted_now();
                for id in &t.orders {
                    self.backoff.remove(id);
                }
                self.txs.insert(t.txid, t);
            }
            SubmitOutcome::DoubleSpend if self.hold_funding(&t, msg.as_deref(), daa) => self.conflicts += 1,
            SubmitOutcome::DoubleSpend | SubmitOutcome::MissingInput => {
                self.conflicts += 1;
                let ids = lost_orders(node, &t, outcome, msg.as_deref()).await;
                self.back_off(&ids, daa);
            }
            SubmitOutcome::Busy => self.back_off(&t.orders, daa),
            SubmitOutcome::Unknown => {
                // The node may have taken it. Follow it like a pending one, and never plan a
                // conflicting transaction over the same inputs.
                self.unknown += 1;
                let mut t = t;
                t.unconfirmed = true;
                self.txs.insert(t.txid, t);
            }
            SubmitOutcome::Rejected => {
                self.rejected += 1;
                let ids = rejected_orders(&self.half_salt, self.rejected, &t.orders);
                self.back_off(&ids, daa);
            }
        }
        outcome
    }

    /// Resends every transaction whose submit got no verdict and that no chain block accepted yet.
    /// The resend is idempotent: the node answers "already in the mempool" for one it holds.
    ///
    /// The node's answer settles the doubt:
    ///
    /// - taken: tracked normally;
    /// - refused: dropped, and its orders back off;
    /// - no answer: it stays unconfirmed.
    ///
    /// A transaction that would no longer be valid ([`Tracker::stale`]) is retired, not sent. A
    /// missing input means its inputs are spent, so it may have been mined while this process was
    /// down. It is retired as doubtful, not counted as dropped, until the store has caught up
    /// ([`Tracker::forget_retired`]).
    ///
    /// Returns the answers.
    pub async fn resubmit_unconfirmed<N: NodeApi>(&mut self, node: &N, clock: &ResendClock) -> Vec<([u8; 32], SubmitOutcome)> {
        let daa = clock.daa;
        let todo: Vec<([u8; 32], Value)> = self
            .txs
            .values()
            .filter(|t| t.unconfirmed && t.accepted.is_none() && t.retired.is_none())
            .map(|t| (t.txid, t.rpc_tx.clone()))
            .collect();
        let mut out = vec![];
        for (id, rpc) in todo {
            if let Some(why) = self.txs.get(&id).and_then(|t| self.stale(t, clock)) {
                self.retire(&id, &why, daa);
                continue;
            }
            let mut msg: Option<String> = None;
            let outcome = match node.submit(rpc).await {
                Ok(_) => SubmitOutcome::Accepted,
                Err(RpcError::Node(m)) => {
                    let o = classify_submit_error(&m);
                    msg = Some(m);
                    o
                }
                Err(_) => SubmitOutcome::Unknown,
            };
            match outcome {
                SubmitOutcome::AlreadyKnown if msg.as_deref().is_some_and(accepted_by_consensus) => {
                    // Accepted by a block this process did not follow (before a restart): done.
                    if let Some(t) = self.txs.remove(&id) {
                        self.done_earlier(&t);
                    }
                }
                SubmitOutcome::Accepted | SubmitOutcome::AlreadyKnown => {
                    if let Some(t) = self.txs.get_mut(&id) {
                        t.unconfirmed = false;
                    }
                    self.checked.insert(id, daa);
                    self.submitted += 1;
                    self.submitted_now();
                }
                SubmitOutcome::Unknown | SubmitOutcome::Busy => {}
                SubmitOutcome::MissingInput if self.restored.contains(&id) => {
                    // spent inputs: mined while this process was down, or a race lost. The store
                    // tells which.
                    if self.hold_retired(&id, daa) {
                        self.doubtful.insert(id);
                        tracing::info!(
                            txid = %kob_protocol::json::to_hex(&id),
                            "a transaction sent again after a restart has spent inputs (mined meanwhile, or a race lost): its acceptance is awaited, not counted as dropped yet"
                        );
                    }
                }
                SubmitOutcome::DoubleSpend | SubmitOutcome::MissingInput | SubmitOutcome::Rejected => {
                    if let Some(t) = self.txs.remove(&id) {
                        self.give_up(node, &t, outcome, msg.as_deref(), daa).await;
                    }
                }
            }
            if !matches!(outcome, SubmitOutcome::Unknown | SubmitOutcome::Busy) {
                self.restored.remove(&id);
            }
            out.push((id, outcome));
        }
        out
    }

    /// A tracked transaction the node refused (`outcome`, `msg`) at a resend: counted, and its
    /// orders back off (none for a held funding coin).
    async fn give_up<N: NodeApi>(&mut self, node: &N, t: &Tracked, outcome: SubmitOutcome, msg: Option<&str>, daa: u64) {
        self.dropped_total += 1;
        self.checked.remove(&t.txid);
        if outcome == SubmitOutcome::Rejected {
            self.rejected += 1;
            let ids = rejected_orders(&self.half_salt, self.rejected, &t.orders);
            self.back_off(&ids, daa);
            return;
        }
        self.conflicts += 1;
        if outcome == SubmitOutcome::DoubleSpend && self.hold_funding(t, msg, daa) {
            return;
        }
        let ids = lost_orders(node, t, outcome, msg).await;
        self.back_off(&ids, daa);
    }

    /// A tracked transaction the node reports already in the chain: done, as a final one (its
    /// profit counts).
    fn done_earlier(&mut self, t: &Tracked) {
        self.checked.remove(&t.txid);
        self.finalized_count += 1;
        self.finalized_profit += t.profit;
        tracing::info!(
            txid = %kob_protocol::json::to_hex(&t.txid),
            kind = %t.kind,
            profit = t.profit,
            "the node reports the transaction accepted by the chain already (by a block this process did not follow): done"
        );
    }

    /// Resends the pending transactions that no chain block accepted for `pending_timeout` DAA
    /// since their submission or their last check. The resend is idempotent.
    ///
    /// The node's answer settles each transaction:
    ///
    /// - still held ("already in the mempool") or taken again: kept and checked again later;
    /// - already in the chain: done;
    /// - refused because an input went elsewhere: dropped, and its orders back off (no order for a
    ///   held funding coin);
    /// - no verdict: tried again at the next check.
    ///
    /// With `may_send` false (the kill switch, a dry run) nothing leaves the process and nothing is
    /// dropped. A transaction whose submission had no verdict is settled by
    /// [`Tracker::resubmit_unconfirmed`] instead.
    ///
    /// A transaction that would no longer be valid ([`Tracker::stale`]: past a day order's
    /// deadline, an order's soft expiry or a sale's freshness) is retired, not sent. A mempool that
    /// still holds it keeps it. Nothing here replaces it.
    pub async fn recheck_pending<N: NodeApi>(&mut self, node: &N, clock: &ResendClock, may_send: bool) -> Recheck {
        let daa = clock.daa;
        let mut r = Recheck::default();
        let txs = &self.txs;
        self.checked.retain(|k, _| txs.contains_key(k));
        if !may_send {
            return r;
        }
        let interval = self.cfg.pending_timeout.max(1);
        let due: Vec<([u8; 32], Value)> = self
            .txs
            .values()
            .filter(|t| t.accepted.is_none() && !t.unconfirmed && t.retired.is_none())
            .filter(|t| {
                daa >= self.checked.get(&t.txid).copied().unwrap_or(t.submitted_daa).max(t.submitted_daa).saturating_add(interval)
            })
            .map(|t| (t.txid, t.rpc_tx.clone()))
            .collect();
        for (id, rpc) in due {
            if let Some(why) = self.txs.get(&id).and_then(|t| self.stale(t, clock)) {
                self.retire(&id, &why, daa);
                r.retired.push(id);
                continue;
            }
            let (outcome, msg) = match node.submit(rpc).await {
                Ok(_) => (SubmitOutcome::Accepted, None),
                Err(RpcError::Node(m)) => (classify_submit_error(&m), Some(m)),
                Err(_) => (SubmitOutcome::Unknown, None),
            };
            match outcome {
                SubmitOutcome::AlreadyKnown if msg.as_deref().is_some_and(accepted_by_consensus) => {
                    if let Some(t) = self.txs.remove(&id) {
                        self.done_earlier(&t);
                        r.accepted_earlier.push(t);
                    }
                }
                SubmitOutcome::Accepted | SubmitOutcome::AlreadyKnown => {
                    self.checked.insert(id, daa);
                    r.kept.push(id);
                    if outcome == SubmitOutcome::Accepted {
                        r.rebroadcast.push(id);
                    }
                }
                SubmitOutcome::Unknown | SubmitOutcome::Busy => {
                    self.checked.insert(id, daa);
                }
                SubmitOutcome::DoubleSpend | SubmitOutcome::MissingInput | SubmitOutcome::Rejected => {
                    if let Some(t) = self.txs.remove(&id) {
                        self.give_up(node, &t, outcome, msg.as_deref(), daa).await;
                        r.dropped.push((t, msg.unwrap_or_default()));
                    }
                }
            }
        }
        r
    }

    /// Drops the pending transactions that lost their race.
    ///
    /// A transaction lost when all of these hold:
    ///
    /// - one of its order inputs ([`Tracked::owners`], listed when the transaction was planned) is
    ///   missing from the book `listed` (the outpoints of every listed order and custody) on two
    ///   calls in a row;
    /// - no chain block accepted the transaction meanwhile;
    /// - the node confirms that input is spent.
    ///
    /// Another transaction spent the input, so this one can never be accepted. It is dropped, the
    /// owner of the spent input backs off, and the other orders are free to be planned again at
    /// once, not after `pending_timeout`. A chained step (its inputs are its parent's outputs) and
    /// a transaction the node cannot tell about are left to the timeout.
    ///
    /// Returns the dropped transactions.
    pub async fn drop_lost<N: NodeApi>(&mut self, node: &N, listed: &BTreeSet<Outpoint>, daa: u64) -> Vec<[u8; 32]> {
        let now: BTreeSet<[u8; 32]> = self
            .txs
            .values()
            .filter(|t| t.accepted.is_none() && t.parent.is_none() && t.owners.keys().any(|op| !listed.contains(op)))
            .map(|t| t.txid)
            .collect();
        let mut dropped = vec![];
        let seen_twice: Vec<[u8; 32]> = now.intersection(&self.lost).copied().collect();
        for txid in seen_twice {
            let Some(t) = self.txs.get(&txid) else { continue };
            if let Some(gone) = missing_owners(node, t).await {
                self.txs.remove(&txid);
                self.checked.remove(&txid);
                self.conflicts += 1;
                self.dropped_total += 1;
                self.back_off(&gone, daa);
                dropped.push(txid);
            }
        }
        self.lost = now.into_iter().filter(|k| !dropped.contains(k)).collect();
        dropped
    }

    /// Applies one chain update at virtual DAA `daa`.
    pub fn apply(&mut self, u: &ChainUpdate, daa: u64) -> TrackerEvents {
        let mut ev = TrackerEvents::default();
        let removed: BTreeSet<&String> = u.removed.iter().collect();
        for t in self.txs.values_mut() {
            if t.accepted.as_ref().is_some_and(|(h, _)| removed.contains(h)) {
                t.accepted = None;
                ev.rolled_back.push(t.txid);
            }
        }
        for b in &u.added {
            for id in &b.accepted {
                if let Some(t) = self.txs.get_mut(id) {
                    t.accepted = Some((b.hash.clone(), b.daa_score));
                    ev.accepted.push(*id);
                }
            }
        }
        if let Some(last) = u.added.last() {
            self.cursor = Some(last.hash.clone());
        }
        self.settle(daa, ev)
    }

    /// Acceptance as seen by a chain follower that is not this tracker (the indexer, in-process).
    ///
    /// `view` maps a transaction id to the chain block that accepts it on the selected chain now
    /// (absent: not accepted). It replaces the VSPC cursor. A transaction whose block was reorged
    /// away is absent and moves back to pending.
    pub fn observe(&mut self, view: &BTreeMap<[u8; 32], (String, u64)>, daa: u64) -> TrackerEvents {
        let mut ev = TrackerEvents::default();
        for t in self.txs.values_mut() {
            match (view.get(&t.txid), &t.accepted) {
                (Some(now), prev) if prev.as_ref() != Some(now) => {
                    t.accepted = Some(now.clone());
                    ev.accepted.push(t.txid);
                }
                (None, Some(_)) => {
                    t.accepted = None;
                    ev.rolled_back.push(t.txid);
                }
                _ => {}
            }
        }
        self.settle(daa, ev)
    }

    /// Final means accepted plus `final_depth`. A transaction no chain block accepted is never
    /// given up here: [`Tracker::recheck_pending`] asks the node about it.
    fn settle(&mut self, daa: u64, mut ev: TrackerEvents) -> TrackerEvents {
        for t in self.txs.values_mut() {
            if t.accepted.is_some() {
                t.unconfirmed = false;
            }
        }
        let done: Vec<[u8; 32]> = self
            .txs
            .iter()
            .filter(|(_, t)| t.accepted.as_ref().is_some_and(|(_, d)| daa >= d + self.cfg.final_depth))
            .map(|(k, _)| *k)
            .collect();
        for k in done {
            let t = self.txs.remove(&k).expect("listed");
            self.checked.remove(&k);
            self.finalized_count += 1;
            self.finalized_profit += t.profit;
            ev.finalized.push(t);
        }
        ev
    }

    /// Transactions rolled back by a reorg that are still pending: resend them (idempotent). A
    /// double spend then means their inputs went elsewhere, and they are dropped at the timeout.
    pub fn resend_candidates(&self) -> Vec<&Tracked> {
        self.txs.values().filter(|t| t.accepted.is_none()).collect()
    }

    /// Pending (not yet accepted) transactions.
    pub fn pending(&self) -> usize {
        self.txs.values().filter(|t| t.accepted.is_none()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matcher::node::ChainBlock;

    /// An order with a low (ground) covenant id is not a fixed victim of a refused batch. The half
    /// that sits out is not the lower half by id, it differs from one refusal to the next, and the
    /// batch still converges on the one order.
    #[test]
    fn the_half_that_sits_out_is_not_the_lowest_ids() {
        let salt = std::collections::hash_map::RandomState::new();
        let batch: BTreeSet<CovId> = (1u8..=8).map(|n| [n; 32]).collect();
        let lowest: BTreeSet<CovId> = batch.iter().take(4).copied().collect();
        let mut seen_other = false;
        let mut firsts = BTreeSet::new();
        for round in 1..=64u64 {
            let out = rejected_orders(&salt, round, &batch);
            assert_eq!(out.len(), 4);
            assert!(out.is_subset(&batch));
            seen_other |= out != lowest;
            firsts.insert(out.into_iter().collect::<Vec<_>>());
        }
        assert!(seen_other && firsts.len() > 3, "the same half every time: {firsts:?}");
        // A single order is the one backed off. Searching converges on the one bad order of eight
        // in at most four refusals.
        assert_eq!(rejected_orders(&salt, 1, &[[9u8; 32]].into_iter().collect()), [[9u8; 32]].into_iter().collect());
        for bad in 1u8..=8 {
            let mut live = batch.clone();
            let mut refusals = 0;
            while live.len() > 1 || refusals == 0 {
                if !live.contains(&[bad; 32]) {
                    break;
                }
                refusals += 1;
                let out = rejected_orders(&salt, refusals, &live);
                if live.len() == 1 {
                    break;
                }
                live = live.difference(&out).copied().collect();
                if refusals > 6 {
                    panic!("no convergence");
                }
            }
            assert!(refusals <= 4, "order {bad}: {refusals} refusals");
        }
    }

    fn tracked(n: u8, daa: u64) -> Tracked {
        Tracked {
            txid: [n; 32],
            spends: [([n; 32], 0)].into_iter().collect(),
            orders: [[n; 32]].into_iter().collect(),
            owners: BTreeMap::new(),
            addresses: BTreeMap::new(),
            parent: None,
            submitted_daa: daa,
            rpc_tx: Value::Null,
            accepted: None,
            kind: "match".into(),
            profit: 7,
            fee: 0,
            fee_rate: 100,
            unconfirmed: false,
            deadline: None,
            resend_until: None,
            retired: None,
        }
    }

    #[test]
    fn accept_finalize_rollback_and_timeout() {
        let mut t = Tracker::new(TrackerConfig::default());
        t.txs.insert([1; 32], tracked(1, 1_000));
        t.txs.insert([2; 32], tracked(2, 1_000));
        assert_eq!(t.spent_outpoints().len(), 2);
        let up =
            ChainUpdate { removed: vec![], added: vec![ChainBlock { hash: "b1".into(), daa_score: 1_010, accepted: vec![[1; 32]] }] };
        let ev = t.apply(&up, 1_011);
        assert_eq!(ev.accepted, vec![[1; 32]]);
        assert_eq!(t.cursor.as_deref(), Some("b1"));
        // Reorg: b1 removed -> back to pending.
        let ev = t.apply(&ChainUpdate { removed: vec!["b1".into()], added: vec![] }, 1_020);
        assert_eq!(ev.rolled_back, vec![[1; 32]]);
        assert_eq!(t.pending(), 2);
        // Re-accepted, then final after 100 DAA. The other transaction is never given up by a chain
        // update (the node is asked).
        let up =
            ChainUpdate { removed: vec![], added: vec![ChainBlock { hash: "b2".into(), daa_score: 1_030, accepted: vec![[1; 32]] }] };
        t.apply(&up, 1_031);
        let ev = t.apply(&ChainUpdate::default(), 1_600);
        assert_eq!(ev.finalized.iter().map(|x| x.txid).collect::<Vec<_>>(), vec![[1; 32]], "accepted at 1030, final from 1130");
        assert_eq!((t.finalized_count, t.finalized_profit), (1, 7));
        assert_eq!(t.txs.keys().collect::<Vec<_>>(), vec![&[2; 32]], "the unaccepted one stays pending");
        assert_eq!(t.oldest_pending_daa(1_600), 600);
    }

    /// A node for [`Tracker::recheck_pending`]: answers each submission from a queue (`None`:
    /// taken).
    #[derive(Default)]
    struct Answers(std::sync::Mutex<std::collections::VecDeque<Option<RpcError>>>, std::sync::atomic::AtomicUsize);

    impl NodeApi for Answers {
        async fn server_info(&self) -> Result<super::super::node::ServerInfo, RpcError> {
            Err(RpcError::Node("unused".into()))
        }
        async fn dag_info(&self) -> Result<super::super::node::DagInfo, RpcError> {
            Err(RpcError::Node("unused".into()))
        }
        async fn submit(&self, _tx: Value) -> Result<[u8; 32], RpcError> {
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match self.0.lock().unwrap().pop_front().flatten() {
                Some(e) => Err(e),
                None => Ok([0; 32]),
            }
        }
        async fn chain_from(&self, _start_hash: &str) -> Result<ChainUpdate, RpcError> {
            Ok(ChainUpdate::default())
        }
        async fn utxos_by_addresses(&self, _a: &[String]) -> Result<Vec<super::super::node::AddressUtxo>, RpcError> {
            Ok(vec![])
        }
    }

    /// An unaccepted transaction is not forgotten after `pending_timeout`: the node is asked (a
    /// resend), and only its refusal drops it. While the node says "already in the mempool" the
    /// inputs stay reserved and the transaction is checked again one interval later. "Already
    /// accepted by the consensus" ends it as done. Nothing is sent while sending is off.
    #[tokio::test]
    async fn an_unaccepted_transaction_is_rechecked_with_the_node_not_forgotten() {
        let node = Answers::default();
        let held = |id: u8| Some(RpcError::Node(format!("transaction {} is already in the mempool", format!("{id:02x}").repeat(32))));
        let mut t = Tracker::new(TrackerConfig::default());
        for n in 1..=4u8 {
            t.txs.insert([n; 32], tracked(n, 1_000));
        }
        // not due yet, and nothing is sent while paused
        assert_eq!(t.recheck_pending(&node, &ResendClock::at(1_599), true).await, Recheck::default());
        assert_eq!(t.recheck_pending(&node, &ResendClock::at(1_600), false).await, Recheck::default());
        assert_eq!(node.1.load(std::sync::atomic::Ordering::SeqCst), 0);
        node.0.lock().unwrap().extend([
            held(1),
            None,
            Some(RpcError::Node(format!("transaction {} was already accepted by the consensus", "03".repeat(32)))),
            Some(RpcError::Node("at least one outpoint of the transaction is lacking a matching UTXO entry".into())),
        ]);
        let r = t.recheck_pending(&node, &ResendClock::at(1_600), true).await;
        assert_eq!(r.kept, vec![[1; 32], [2; 32]]);
        assert_eq!(r.rebroadcast, vec![[2; 32]], "the node no longer held it and took it again");
        assert_eq!(r.accepted_earlier.iter().map(|x| x.txid).collect::<Vec<_>>(), vec![[3; 32]]);
        assert_eq!(r.dropped.iter().map(|(x, _)| x.txid).collect::<Vec<_>>(), vec![[4; 32]]);
        assert_eq!((t.dropped_total, t.finalized_count, t.finalized_profit), (1, 1, 7));
        assert!(t.backed_off(1_600).contains(&[4; 32]) && !t.backed_off(1_600).contains(&[1; 32]));
        assert!(t.spent_outpoints().contains(&([1; 32], 0)), "the held one's inputs stay reserved");
        // the kept ones are checked again one interval after their check, not every step
        assert_eq!(t.recheck_pending(&node, &ResendClock::at(2_100), true).await, Recheck::default());
        node.0.lock().unwrap().extend([held(1), held(2)]);
        assert_eq!(t.recheck_pending(&node, &ResendClock::at(2_200), true).await.kept, vec![[1; 32], [2; 32]]);
        assert_eq!(t.oldest_pending_daa(2_200), 1_200);
    }

    /// A resend is checked first. A pending transaction past a day order's deadline (or within the
    /// clock margin of it), past an order's soft expiry or a sale's freshness, or a submission
    /// without a verdict older than `max_resend_age`, is not sent. It is retired (counted), its
    /// inputs stay reserved for `hold_daa` and its acceptance is still followed. Then it is
    /// forgotten.
    #[tokio::test]
    async fn a_resend_that_would_no_longer_be_valid_is_not_sent() {
        let node = Answers::default();
        let sent = || node.1.load(std::sync::atomic::Ordering::SeqCst);
        let mut t = Tracker::new(TrackerConfig::default());
        let day = Tracked { deadline: Some(10_000), ..tracked(1, 1_000) };
        let expiring = Tracked { resend_until: Some(1_500), ..tracked(2, 1_000) };
        let fine = Tracked { deadline: Some(20_000), resend_until: Some(9_000), ..tracked(3, 1_000) };
        for x in [day, expiring, fine] {
            t.txs.insert(x.txid, x);
        }
        // The deadline is 30 s away, within the margin, so that transaction is not sent. The
        // expired one is not sent either. The third is.
        let clock = ResendClock { daa: 1_600, utc: 9_970, margin_secs: 30 };
        let r = t.recheck_pending(&node, &clock, true).await;
        assert_eq!(r.retired, vec![[1; 32], [2; 32]]);
        assert_eq!((r.kept, sent()), (vec![[3; 32]], 1), "only the valid one was sent");
        assert_eq!((t.resend_dropped_total, t.dropped_total), (2, 0));
        assert!(t.spent_outpoints().contains(&([1; 32], 0)), "a mempool may still hold it: its inputs stay reserved");
        assert_eq!(t.oldest_pending_daa(1_600), 600, "a retired one is not waited for");
        // never sent again, however long it waits
        let later = ResendClock { daa: 2_300, utc: 10_100, margin_secs: 30 };
        assert!(t.recheck_pending(&node, &later, true).await.retired.is_empty());
        assert_eq!(sent(), 2, "the valid one again, the retired ones never");
        // the chain shows one of them accepted after all: followed to the end, its profit counts
        let seen: BTreeMap<[u8; 32], (String, u64)> = [([2; 32], ("b".to_string(), 2_250))].into_iter().collect();
        t.observe(&seen, 2_300);
        // the hold ends: the unaccepted one is forgotten, the accepted one is kept
        assert!(t.forget_retired(4_599).is_empty());
        let gone = t.forget_retired(4_600);
        assert_eq!(gone.iter().map(|x| x.txid).collect::<Vec<_>>(), vec![[1; 32]]);
        assert!(t.txs.contains_key(&[2; 32]) && !t.spent_outpoints().contains(&([1; 32], 0)));
        assert_eq!(t.dropped_total, 0, "not a refusal: counted apart");

        // A submission without a verdict older than `max_resend_age` (a journal entry of a long
        // downtime) is not sent blindly.
        let mut t = Tracker::new(TrackerConfig::default());
        t.txs.insert([5; 32], Tracked { unconfirmed: true, ..tracked(5, 1_000) });
        t.txs.insert([6; 32], Tracked { unconfirmed: true, ..tracked(6, 7_000) });
        let before = sent();
        let out = t.resubmit_unconfirmed(&node, &ResendClock::at(7_001)).await;
        assert_eq!(out.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![[6; 32]], "only the recent one is resent");
        assert_eq!((sent() - before, t.resend_dropped_total), (1, 1));
        assert!(t.txs[&[5; 32]].retired.is_some());
    }

    /// A journal entry refused for a missing input after a restart (its inputs are spent, so it may
    /// have been mined while the process was down) is not counted as dropped at once. It waits for
    /// the store, which shows it accepted (its profit counts) or not (then it counts as dropped). A
    /// submission of this run refused the same way is given up.
    #[tokio::test]
    async fn a_restored_transaction_mined_during_the_downtime_is_not_counted_as_dropped() {
        let node = Answers::default();
        let missing = || Some(RpcError::Node("at least one outpoint of the transaction is lacking a matching UTXO entry".into()));
        node.0.lock().unwrap().extend([missing(), missing(), missing()]);
        let mut t = Tracker::new(TrackerConfig::default());
        for n in 1..=3u8 {
            t.txs.insert([n; 32], Tracked { unconfirmed: true, ..tracked(n, 1_000) });
        }
        t.restored = [[1; 32], [2; 32]].into_iter().collect();
        t.resubmit_unconfirmed(&node, &ResendClock::at(1_100)).await;
        assert_eq!(t.dropped_total, 1, "this run's own submission is given up as before");
        assert!(!t.txs.contains_key(&[3; 32]));
        assert_eq!(t.doubtful, [[1; 32], [2; 32]].into_iter().collect());
        assert!(t.restored.is_empty(), "the node answered about each");
        // the store catches up: one was mined during the downtime
        let seen: BTreeMap<[u8; 32], (String, u64)> = [([1; 32], ("b".to_string(), 900))].into_iter().collect();
        let ev = t.observe(&seen, 1_200);
        assert_eq!(ev.finalized.iter().map(|x| x.txid).collect::<Vec<_>>(), vec![[1; 32]]);
        assert_eq!((t.finalized_count, t.finalized_profit), (1, 7));
        // the other was not: dropped once its hold ends
        t.forget_retired(4_100);
        assert_eq!((t.dropped_total, t.resend_dropped_total), (2, 0));
        assert!(t.txs.is_empty() && t.doubtful.is_empty());
    }

    /// A mempool double spend of the operator's own funding coin holds the coin (out of the
    /// funding) and backs off no order. The hold ends when the node no longer lists the coin, or
    /// after `hold_daa`.
    #[tokio::test]
    async fn a_double_spend_of_a_funding_coin_holds_the_coin_not_the_orders() {
        let coin: Outpoint = ([0x77; 32], 1);
        let ds = format!("output ({}, 1) already spent by transaction {} in the mempool", "77".repeat(32), "ee".repeat(32));
        let node = Answers::default();
        node.0.lock().unwrap().push_back(Some(RpcError::Node(ds.clone())));
        let mut t = Tracker::new(TrackerConfig::default());
        t.note_operator_coins([coin, ([0x78; 32], 0)].into_iter().collect(), 100);
        let mut x = tracked(1, 100);
        x.spends.insert(coin);
        assert_eq!(t.submit(&node, x.clone(), 100).await, SubmitOutcome::DoubleSpend);
        assert!(t.backed_off(100).is_empty(), "no order backs off");
        assert_eq!((t.held.get(&coin).copied(), t.held_total), (Some(3_100), 1));
        assert!(t.spent_outpoints().contains(&coin), "the coin is out of the funding");
        // still listed: held; no longer listed (its spender was mined): released
        t.note_operator_coins([coin].into_iter().collect(), 200);
        assert!(t.held.contains_key(&coin));
        t.note_operator_coins(BTreeSet::new(), 300);
        assert!(t.held.is_empty());
        // or the hold runs out
        node.0.lock().unwrap().push_back(Some(RpcError::Node(ds)));
        t.note_operator_coins([coin].into_iter().collect(), 400);
        t.submit(&node, x, 400).await;
        t.note_operator_coins([coin].into_iter().collect(), 3_399);
        assert!(t.held.contains_key(&coin));
        t.note_operator_coins([coin].into_iter().collect(), 3_400);
        assert!(t.held.is_empty());
    }

    /// Backoffs that ran out long ago are forgotten.
    #[test]
    fn expired_backoffs_are_pruned() {
        let mut t = Tracker::new(TrackerConfig::default());
        t.back_off(&[[1; 32]].into_iter().collect(), 100);
        t.back_off(&[[2; 32]].into_iter().collect(), 1_000);
        t.prune_backoff(719);
        assert_eq!(t.backoff.len(), 2);
        t.prune_backoff(720);
        assert_eq!(t.backoff.keys().collect::<Vec<_>>(), vec![&[2; 32]]);
    }

    #[test]
    fn observed_acceptance_replaces_the_vspc_cursor() {
        let mut t = Tracker::new(TrackerConfig::default());
        t.txs.insert([1; 32], tracked(1, 1_000));
        t.txs.insert([2; 32], tracked(2, 1_000));
        let seen = |pairs: &[([u8; 32], &str, u64)]| -> BTreeMap<[u8; 32], (String, u64)> {
            pairs.iter().map(|(id, h, d)| (*id, (h.to_string(), *d))).collect()
        };
        // accepted by the indexer
        let ev = t.observe(&seen(&[([1; 32], "b1", 1_010)]), 1_011);
        assert_eq!(ev.accepted, vec![[1; 32]]);
        assert_eq!(t.pending(), 1);
        // the same view again is no event
        assert_eq!(t.observe(&seen(&[([1; 32], "b1", 1_010)]), 1_012), TrackerEvents::default());
        // re-accepted by another chain block in the same poll: accepted, not rolled back
        let ev = t.observe(&seen(&[([1; 32], "b2", 1_020)]), 1_021);
        assert_eq!((ev.accepted.clone(), ev.rolled_back.len()), (vec![[1; 32]], 0));
        assert_eq!(t.txs[&[1; 32]].accepted, Some(("b2".to_string(), 1_020)));
        // absent from the view: reorged away, back to pending
        let ev = t.observe(&seen(&[]), 1_030);
        assert_eq!(ev.rolled_back, vec![[1; 32]]);
        assert_eq!(t.pending(), 2);
        // Accepted again, final 100 DAA later. The unaccepted one stays pending (the node is asked
        // about it, not the view).
        t.observe(&seen(&[([1; 32], "b3", 1_040)]), 1_041);
        let ev = t.observe(&seen(&[([1; 32], "b3", 1_040)]), 1_600);
        assert_eq!(ev.finalized.iter().map(|x| x.txid).collect::<Vec<_>>(), vec![[1; 32]]);
        assert_eq!(t.txs.keys().collect::<Vec<_>>(), vec![&[2; 32]]);
    }

    #[test]
    fn a_double_spend_names_its_outpoint() {
        let id = "ab".repeat(32);
        let want = Some(([0xab; 32], 3));
        // rusty-kaspa's mempool rule error, as the RPC wraps it. `TransactionOutpoint` displays as
        // `(<txid>, <index>)`.
        let m = format!("Rejected transaction {id}: output ({id}, 3) already spent by transaction {} in the mempool", "cd".repeat(32));
        assert_eq!(double_spent_outpoint(&m), want);
        assert_eq!(double_spent_outpoint(&format!("output {id}:3 already spent by transaction ee in the mempool")), want);
        assert_eq!(double_spent_outpoint("output ab:0 already spent by transaction cd in the mempool"), None, "not a txid");
        assert_eq!(double_spent_outpoint("transaction is an orphan"), None);
    }

    #[test]
    fn a_double_spend_backs_off_only_the_order_whose_input_it_names() {
        let mut t = tracked(1, 100);
        t.orders = [[1; 32], [2; 32], [3; 32]].into_iter().collect();
        t.owners = [(([0x11; 32], 0), [1; 32]), (([0x22; 32], 1), [2; 32])].into_iter().collect();
        let ds = |op: &str| format!("output {op} already spent by transaction {} in the mempool", "ee".repeat(32));
        let named = ds(&format!("({}, 1)", "22".repeat(32)));
        let only = |ids: BTreeSet<CovId>| ids.into_iter().collect::<Vec<_>>();
        assert_eq!(only(conflict_orders(&t, SubmitOutcome::DoubleSpend, Some(&named))), vec![[2; 32]]);
        // An input no order owns (the operator's funding), an unparsable message, or no message:
        // every order.
        let funding = ds(&format!("({}, 0)", "99".repeat(32)));
        assert_eq!(conflict_orders(&t, SubmitOutcome::DoubleSpend, Some(&funding)), t.orders);
        assert_eq!(conflict_orders(&t, SubmitOutcome::DoubleSpend, Some("double spend")), t.orders);
        assert_eq!(conflict_orders(&t, SubmitOutcome::DoubleSpend, None), t.orders);
        // a missing input names nothing: every order
        assert_eq!(conflict_orders(&t, SubmitOutcome::MissingInput, Some(&named)), t.orders);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let mut t = Tracker::new(TrackerConfig::default());
        let ids: BTreeSet<CovId> = [[9; 32]].into_iter().collect();
        t.back_off(&ids, 100);
        assert_eq!(t.backoff[&[9; 32]], (120, 1));
        t.back_off(&ids, 100);
        assert_eq!(t.backoff[&[9; 32]], (140, 2));
        for _ in 0..10 {
            t.back_off(&ids, 100);
        }
        assert_eq!(t.backoff[&[9; 32]].0, 700);
    }

    /// A retired transaction's inputs are free before its hold ends once the nodes say twice in a
    /// row, with checks at least `RETIRED_CHECK_DAA` apart, that their mempools do not hold it. A
    /// held or an unsure answer keeps the hold (and begins the misses again). An accepted
    /// transaction is never released this way.
    #[test]
    fn a_retired_transaction_no_mempool_holds_is_released_before_its_hold_ends() {
        let mut t = Tracker::new(TrackerConfig::default());
        for n in 1..=3u8 {
            t.txs.insert([n; 32], tracked(n, 1_000));
            t.retire(&[n; 32], "test", 1_000);
        }
        t.txs.get_mut(&[3; 32]).unwrap().accepted = Some(("b".into(), 1_050));
        t.doubtful.insert([2; 32]);
        assert_eq!(t.retired_to_check(1_000), vec![[1; 32], [2; 32]], "the accepted one is followed as any other");
        // first miss: kept, and not asked again within RETIRED_CHECK_DAA
        assert!(t.retired_checked(&[1; 32], Some(false), 1_000).is_none());
        assert!(t.retired_checked(&[2; 32], Some(false), 1_000).is_none());
        assert!(t.retired_to_check(1_000 + RETIRED_CHECK_DAA - 1).is_empty());
        let later = 1_000 + RETIRED_CHECK_DAA;
        assert_eq!(t.retired_to_check(later), vec![[1; 32], [2; 32]]);
        // First: unsure (the node failed) keeps the hold and begins the misses again. Second: a
        // second miss releases it.
        assert!(t.retired_checked(&[1; 32], None, later).is_none());
        let gone = t.retired_checked(&[2; 32], Some(false), later).expect("released");
        assert_eq!(gone.txid, [2; 32]);
        assert!(!t.spent_outpoints().contains(&([2; 32], 0)), "its inputs are free again");
        assert_eq!(t.dropped_total, 1, "a doubtful one counts as dropped, as at the hold's end");
        // the first: held now (the node has it again), then two misses
        let at = |k: u64| later + k * RETIRED_CHECK_DAA;
        assert!(t.retired_checked(&[1; 32], Some(true), at(1)).is_none());
        assert!(t.retired_checked(&[1; 32], Some(false), at(2)).is_none());
        assert!(t.retired_checked(&[1; 32], Some(false), at(3)).is_some());
        assert_eq!(t.txs.keys().copied().collect::<Vec<_>>(), vec![[3; 32]]);
        assert!(t.retired_misses.is_empty());
        // past its hold: not asked (forget_retired takes it)
        let mut u = Tracker::new(TrackerConfig::default());
        u.txs.insert([4; 32], tracked(4, 1_000));
        u.retire(&[4; 32], "test", 1_000);
        assert!(u.retired_to_check(1_000 + u.cfg.hold_daa).is_empty());
    }

    #[test]
    fn only_a_transaction_not_found_answer_is_a_mempool_miss() {
        let id = [0xab; 32];
        let hex = kob_protocol::json::to_hex(&id);
        assert!(super::super::node::mempool_miss(&format!("Transaction {hex} not found"), &id));
        assert!(!super::super::node::mempool_miss("Method not found", &id));
        assert!(!super::super::node::mempool_miss(&format!("Transaction {} not found", "cd".repeat(32)), &id));
        assert!(!super::super::node::mempool_miss("connection closed", &id));
    }
}
