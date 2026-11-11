//! Submission, mempool conflicts, acceptance tracking and rollback (`docs/spec/matcher.md` §1.3,
//! §7, §8).
//!
//! * Every transaction is engine-validated before it reaches [`Tracker::submit`].
//! * `RejectDoubleSpendInMempool` is benign (a competing matcher, a cancel, a refund): the order
//!   whose input the node names backs off exponentially (every order of the transaction when the
//!   input is no order's, [`Tracked::owners`]); nothing is ever replaced blindly (the matcher never
//!   calls `submitTransactionReplacement`; the runner replaces only its own stuck keeper kills and
//!   refunds, `crate::matcher::run::Runner::replace_stuck`).
//! * A missing input (an orphan: the input's spend is already in a block the book has not shown
//!   yet) names no outpoint: the tracker asks the node which of the order inputs are still unspent
//!   ([`Tracked::addresses`]) and backs off the orders whose inputs are gone; the other orders of
//!   the transaction are planned again at once (every order backs off only when the node cannot
//!   tell).
//! * Acceptance comes from `getVirtualChainFromBlockV2` (`Low`): a transaction is *accepted* when a
//!   chain block accepts it and *final* (done) after `final_depth` more DAA; a `removed` chain
//!   block moves its transactions back to pending (reorg), where they are re-checked.
//! * While a transaction is pending or accepted-but-not-final, the outpoints it spends are
//!   excluded from planning, so no order is planned twice. A pending transaction no chain block accepted
//!   within `pending_timeout` DAA is not forgotten: it is resent to the node, which settles it
//!   ([`Tracker::recheck_pending`]). "Already in the mempool" (or taken again): it stays pending, its inputs
//!   stay reserved, and it is checked again after another `pending_timeout` (a node keeps the
//!   transactions submitted to it until they are mined or evicted, so forgetting one would make its own
//!   funding coin refuse every later batch as a mempool double spend). Refused (its inputs went
//!   elsewhere): it is dropped (logged, `dropped_total`) and its orders back off.
//!   A pending transaction one of whose order inputs the book no longer lists on two steps in a row
//!   and the node confirms spent (another transaction spent it: the race is lost) is dropped at once
//!   ([`Tracker::drop_lost`]): the order whose input is gone backs off, the others are planned again.
//! * A mempool double spend of one of the operator's own funding coins (a transaction this process does
//!   not know holds it: one from before a restart that the journal missed, a withdraw, a second process on
//!   the same key) backs off no order: the coin is held out of the funding for `hold_daa`, or until the
//!   node's UTXO set no longer lists it ([`Tracker::held`]), and the batch is planned again at once over
//!   the other coins.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::book::{CovId, Outpoint};
use super::node::{classify_submit_error, ChainUpdate, NodeApi, RpcError, SubmitOutcome};

/// Tracker settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackerConfig {
    /// DAA after acceptance at which a transaction is final (default 100, §1.3).
    pub final_depth: u64,
    /// DAA after which an unaccepted transaction is checked with the node by an idempotent resend, and again every as many DAA
    /// while the node holds it (default 600, a minute; [`Tracker::recheck_pending`]).
    pub pending_timeout: u64,
    /// First back-off of an order after a conflict (DAA; doubles per attempt).
    pub backoff_base: u64,
    /// Longest back-off (DAA).
    pub backoff_max: u64,
    /// DAA a funding coin of the operator that a mempool transaction unknown to the tracker spends stays out of the funding
    /// (default 3000, five minutes; earlier when the node's UTXO set no longer lists it).
    pub hold_daa: u64,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        TrackerConfig { final_depth: 100, pending_timeout: 600, backoff_base: 20, backoff_max: 600, hold_daa: 3_000 }
    }
}

/// Whether a node's refusal says the transaction is already in the chain (rusty-kaspa `RejectAlreadyAccepted`): accepted
/// earlier, by a block this process may not have followed (before a restart).
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
    /// Transactions the node refused (their inputs went elsewhere), with its answer: dropped, their orders back off.
    pub dropped: Vec<(Tracked, String)>,
    /// Transactions the node reports already accepted by the chain (before this process followed it): done.
    pub accepted_earlier: Vec<Tracked>,
}

/// A transaction the tracker follows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tracked {
    pub txid: [u8; 32],
    /// Outpoints the transaction spends.
    pub spends: BTreeSet<Outpoint>,
    /// Order covenant ids it spends (legs, merged entries).
    pub orders: BTreeSet<CovId>,
    /// The order each of its inputs belongs to, where known (an order's own UTXO, its custody): a mempool double spend of one
    /// of these outpoints backs off that order only. Empty, or a conflict on any other input: every order of the transaction
    /// backs off.
    pub owners: BTreeMap<Outpoint, CovId>,
    /// The address of each input `owners` names (where known): on a missing input the node is asked which of them are still
    /// unspent.
    pub addresses: BTreeMap<Outpoint, String>,
    /// The transaction's own outpoints its children spend (chained steps).
    pub parent: Option<[u8; 32]>,
    pub submitted_daa: u64,
    /// `RpcTransaction` JSON, for an idempotent resend after a reorg.
    pub rpc_tx: Value,
    /// Accepting chain block and its DAA score.
    pub accepted: Option<(String, u64)>,
    /// What the transaction is (for logs and metrics): `match`, `refund`, `kill`, `close`, `sweep`.
    pub kind: String,
    /// Operator profit promised by the build (sompi).
    pub profit: i64,
    /// Network fee paid (sompi) and its rate (sompi per gram, `crate::fee`).
    pub fee: u64,
    pub fee_rate: u64,
    /// The submit call got no verdict (transport error / timeout): the node may hold the transaction. Cleared by the node's answer
    /// to a resend or by the transaction's acceptance.
    pub unconfirmed: bool,
}

/// Events of one tracker step (for logs and metrics).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackerEvents {
    pub accepted: Vec<[u8; 32]>,
    pub finalized: Vec<Tracked>,
    pub rolled_back: Vec<[u8; 32]>,
}

/// The outpoint a node's double-spend rejection names: rusty-kaspa's `output (<txid>, <index>) already spent by transaction
/// <txid> in the mempool` (`TransactionOutpoint` displays as `(<txid>, <index>)`), or `output <txid>:<index> ...`.
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

/// The orders a lost race backs off: the one whose input a double spend names, else (a missing input) those whose inputs the
/// node no longer holds, else every order of the transaction.
async fn lost_orders<N: NodeApi>(node: &N, t: &Tracked, outcome: SubmitOutcome, msg: Option<&str>) -> BTreeSet<CovId> {
    if outcome == SubmitOutcome::MissingInput {
        if let Some(ids) = missing_owners(node, t).await {
            return ids;
        }
    }
    conflict_orders(t, outcome, msg)
}

/// The orders a refused submission backs off: on a mempool double spend of an input whose order is known
/// ([`Tracked::owners`]), that order alone (the others in the transaction did nothing wrong and are planned again at once);
/// otherwise every order of the transaction.
fn conflict_orders(t: &Tracked, outcome: SubmitOutcome, msg: Option<&str>) -> BTreeSet<CovId> {
    if outcome == SubmitOutcome::DoubleSpend {
        if let Some(id) = msg.and_then(double_spent_outpoint).and_then(|op| t.owners.get(&op)) {
            return [*id].into_iter().collect();
        }
    }
    t.orders.clone()
}

/// The orders a node's generic refusal ([`SubmitOutcome::Rejected`]) backs off. The refusal names no order, so the batch is
/// searched by halves across ticks: half of a transaction with several orders sits out, the other half is planned again at
/// once and, if the node refuses it again, loses half of itself the same way, until a single order is the one backed off. So one
/// order that makes a node-only policy refuse every batch it is in costs the others at most one backoff each (audit 2026-10-30
/// E2), never the whole batch every time. The half is chosen by a keyed hash of the order id and the refusal count, the key random
/// per process: it was the lower half by id, which an order with a ground low covenant id (cheap to make) made a fixed victim
/// of the lowest honest ids of every batch it was in (audit 2026-10-30 E3).
fn rejected_orders(salt: &std::collections::hash_map::RandomState, round: u64, orders: &BTreeSet<CovId>) -> BTreeSet<CovId> {
    use std::hash::BuildHasher;
    let half = orders.len().div_ceil(2);
    let mut keyed: Vec<(u64, CovId)> = orders.iter().map(|id| (salt.hash_one((id, round)), *id)).collect();
    keyed.sort_unstable();
    keyed.into_iter().take(half).map(|(_, id)| id).collect()
}

/// The orders of `t` whose inputs the node no longer holds, asked by address after a missing-input refusal: None when the
/// node cannot tell (no address known, a node without its UTXO index (`--utxoindex`: an address query proves nothing
/// there, so nothing is taken as spent), the query failed, or every order input is still there: the missing input is
/// another one).
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
    pub cfg: TrackerConfig,
    /// Last chain block seen (VSPC cursor).
    pub cursor: Option<String>,
    pub txs: BTreeMap<[u8; 32], Tracked>,
    /// Order id -> (back off until DAA, attempts).
    pub backoff: BTreeMap<CovId, (u64, u32)>,
    /// The key of the keyed hash that picks the half of a refused batch to back off ([`rejected_orders`]): random per process.
    half_salt: std::collections::hash_map::RandomState,
    /// Counters.
    pub submitted: u64,
    pub conflicts: u64,
    pub rejected: u64,
    /// Submits without a verdict.
    pub unknown: u64,
    pub finalized_count: u64,
    pub finalized_profit: i64,
    /// Pending transactions one of whose order inputs the last book did not list ([`Tracker::drop_lost`]).
    pub lost: BTreeSet<[u8; 32]>,
    /// The operator's funding coins (its P2PK outpoints) at the last funding read ([`Tracker::note_operator_coins`]): a mempool
    /// double spend naming one of them holds the coin instead of backing off the batch's orders.
    pub operator_coins: BTreeSet<Outpoint>,
    /// Funding coins a mempool transaction unknown to the tracker spends, and the DAA score until which they stay out of the
    /// funding (`TrackerConfig::hold_daa`; dropped earlier once the node's UTXO set no longer lists them).
    pub held: BTreeMap<Outpoint, u64>,
    /// DAA score at which each pending transaction was last checked with the node ([`Tracker::recheck_pending`]).
    pub checked: BTreeMap<[u8; 32], u64>,
    /// Transactions given up: refused by the node at a recheck or a resend, or lost a race ([`Tracker::drop_lost`]).
    pub dropped_total: u64,
    /// Funding coins held after a mempool double spend ([`Tracker::held`]).
    pub held_total: u64,
    /// Unix time (seconds) of the last submission the node took (0: none yet).
    pub last_submit_unix: u64,
}

impl Tracker {
    pub fn new(cfg: TrackerConfig) -> Self {
        Tracker { cfg, ..Default::default() }
    }

    /// Outpoints no plan may spend (pending and accepted-but-not-final transactions, and held funding coins).
    pub fn spent_outpoints(&self) -> BTreeSet<Outpoint> {
        let mut s: BTreeSet<Outpoint> = self.txs.values().flat_map(|t| t.spends.iter().copied()).collect();
        s.extend(self.held.keys().copied());
        s
    }

    /// The operator's funding coins as the node lists them now: a held coin the node no longer lists (its spender was mined, or
    /// it is gone) and one whose hold ran out are released.
    pub fn note_operator_coins(&mut self, coins: BTreeSet<Outpoint>, daa: u64) {
        self.held.retain(|op, until| *until > daa && coins.contains(op));
        self.operator_coins = coins;
    }

    /// Order ids backed off at `daa`.
    pub fn backed_off(&self, daa: u64) -> BTreeSet<CovId> {
        self.backoff.iter().filter(|(_, (until, _))| *until > daa).map(|(k, _)| *k).collect()
    }

    /// Forgets the backoffs that ran out longer than `backoff_max` ago (their attempt count no longer doubles anything): the
    /// map stays as small as the orders that recently lost a race.
    pub fn prune_backoff(&mut self, daa: u64) {
        let keep = self.cfg.backoff_max;
        self.backoff.retain(|_, (until, _)| until.saturating_add(keep) > daa);
    }

    /// DAA the oldest pending (unaccepted) transaction has waited at `daa` (0: none pending).
    pub fn oldest_pending_daa(&self, daa: u64) -> u64 {
        self.txs.values().filter(|t| t.accepted.is_none()).map(|t| daa.saturating_sub(t.submitted_daa)).max().unwrap_or(0)
    }

    /// A mempool double spend of `t` that names one of the operator's funding coins (`msg`): the coin is held
    /// ([`Tracker::held`]) and no order backs off. `false`: the double spend is about something else.
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
                // A chained step whose parent failed: never submit it.
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
                // The node may have taken it: never plan a conflicting transaction over the same inputs; follow it like a pending one.
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

    /// Resends every transaction whose submit got no verdict and that no chain block accepted yet (idempotent: the node answers
    /// "already in the mempool" for one it holds). The node's answer settles the doubt: taken -> tracked normally; refused -> dropped
    /// and its orders back off; still no answer -> stays unconfirmed. Returns the answers.
    pub async fn resubmit_unconfirmed<N: NodeApi>(&mut self, node: &N, daa: u64) -> Vec<([u8; 32], SubmitOutcome)> {
        let todo: Vec<([u8; 32], Value)> =
            self.txs.values().filter(|t| t.unconfirmed && t.accepted.is_none()).map(|t| (t.txid, t.rpc_tx.clone())).collect();
        let mut out = vec![];
        for (id, rpc) in todo {
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
                    // accepted by a block this process did not follow (before a restart): done
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
                SubmitOutcome::DoubleSpend | SubmitOutcome::MissingInput | SubmitOutcome::Rejected => {
                    if let Some(t) = self.txs.remove(&id) {
                        self.give_up(node, &t, outcome, msg.as_deref(), daa).await;
                    }
                }
            }
            out.push((id, outcome));
        }
        out
    }

    /// A tracked transaction the node refused (`outcome`, `msg`) at a resend: counted, and its orders back off (none for a held
    /// funding coin).
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

    /// A tracked transaction the node reports already in the chain: done, as a final one (its profit counts).
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

    /// Pending transactions no chain block accepted for `pending_timeout` DAA since their submission or their last check are
    /// resent (idempotent) and the node's answer settles each: still held ("already in the mempool") or taken again: kept and
    /// checked again later; already in the chain: done; refused (an input went elsewhere): dropped, its orders back off (no
    /// order for a held funding coin); no verdict: tried again at the next check. Not while `may_send` is false (the kill
    /// switch, a dry run): nothing leaves the process then, and nothing is dropped either. A transaction whose submission had
    /// no verdict is settled by [`Tracker::resubmit_unconfirmed`] instead.
    pub async fn recheck_pending<N: NodeApi>(&mut self, node: &N, daa: u64, may_send: bool) -> Recheck {
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
            .filter(|t| t.accepted.is_none() && !t.unconfirmed)
            .filter(|t| {
                daa >= self.checked.get(&t.txid).copied().unwrap_or(t.submitted_daa).max(t.submitted_daa).saturating_add(interval)
            })
            .map(|t| (t.txid, t.rpc_tx.clone()))
            .collect();
        for (id, rpc) in due {
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

    /// Drops the pending transactions that lost their race: one of their order inputs ([`Tracked::owners`], listed when the
    /// transaction was planned) is missing from the book `listed` (the outpoints of every listed order and custody) on two
    /// calls in a row, no chain block accepted the transaction meanwhile, and the node confirms that input is spent. Another
    /// transaction spent it, so this one can never be accepted: it is dropped, the owner of the spent input backs off, and the
    /// other orders are free to be planned again (instead of waiting `pending_timeout` and backing off). A chained step (its
    /// inputs are its parent's outputs) and a transaction the node cannot tell about are left to the timeout. Returns the
    /// dropped transactions.
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

    /// Acceptance as seen by a chain follower that is not this tracker (the indexer, in-process):
    /// `view` maps a transaction id to the chain block that accepts it on the selected chain now
    /// (absent: not accepted). It replaces the VSPC cursor: a transaction whose block was reorged
    /// away is simply absent, and moves back to pending.
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

    /// Final: accepted + depth. A transaction no chain block accepted is never given up here: [`Tracker::recheck_pending`]
    /// asks the node about it.
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

    /// Transactions rolled back by a reorg that are still pending: resend them (idempotent; a
    /// double spend now means their inputs went elsewhere, and they are dropped at the timeout).
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

    /// Audit 2026-10-30 E3: an order with a low (ground) covenant id is not a fixed victim of a refused batch: the half that sits out is
    /// not the lower half by id, it differs from one refusal to the next, and the batch still converges on the one order.
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
        // a single order is the one backed off; searching converges on the one bad order of eight in at most three refusals
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
        // Re-accepted, then final after 100 DAA; the other one is never given up by a chain update (the node is asked).
        let up =
            ChainUpdate { removed: vec![], added: vec![ChainBlock { hash: "b2".into(), daa_score: 1_030, accepted: vec![[1; 32]] }] };
        t.apply(&up, 1_031);
        let ev = t.apply(&ChainUpdate::default(), 1_600);
        assert_eq!(ev.finalized.iter().map(|x| x.txid).collect::<Vec<_>>(), vec![[1; 32]], "accepted at 1030, final from 1130");
        assert_eq!((t.finalized_count, t.finalized_profit), (1, 7));
        assert_eq!(t.txs.keys().collect::<Vec<_>>(), vec![&[2; 32]], "the unaccepted one stays pending");
        assert_eq!(t.oldest_pending_daa(1_600), 600);
    }

    /// A node for [`Tracker::recheck_pending`]: answers each submission from a queue (`None`: taken).
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

    /// Audit 2026-11-11 M-2: an unaccepted transaction is not forgotten after `pending_timeout`: the node is asked (a resend),
    /// and only its refusal drops it. While it says "already in the mempool" the inputs stay reserved and it is checked again
    /// one interval later; "already accepted by the consensus" ends it as done; nothing is sent while sending is off.
    #[tokio::test]
    async fn an_unaccepted_transaction_is_rechecked_with_the_node_not_forgotten() {
        let node = Answers::default();
        let held = |id: u8| Some(RpcError::Node(format!("transaction {} is already in the mempool", format!("{id:02x}").repeat(32))));
        let mut t = Tracker::new(TrackerConfig::default());
        for n in 1..=4u8 {
            t.txs.insert([n; 32], tracked(n, 1_000));
        }
        // not due yet, and nothing is sent while paused
        assert_eq!(t.recheck_pending(&node, 1_599, true).await, Recheck::default());
        assert_eq!(t.recheck_pending(&node, 1_600, false).await, Recheck::default());
        assert_eq!(node.1.load(std::sync::atomic::Ordering::SeqCst), 0);
        node.0.lock().unwrap().extend([
            held(1),
            None,
            Some(RpcError::Node(format!("transaction {} was already accepted by the consensus", "03".repeat(32)))),
            Some(RpcError::Node("at least one outpoint of the transaction is lacking a matching UTXO entry".into())),
        ]);
        let r = t.recheck_pending(&node, 1_600, true).await;
        assert_eq!(r.kept, vec![[1; 32], [2; 32]]);
        assert_eq!(r.rebroadcast, vec![[2; 32]], "the node no longer held it and took it again");
        assert_eq!(r.accepted_earlier.iter().map(|x| x.txid).collect::<Vec<_>>(), vec![[3; 32]]);
        assert_eq!(r.dropped.iter().map(|(x, _)| x.txid).collect::<Vec<_>>(), vec![[4; 32]]);
        assert_eq!((t.dropped_total, t.finalized_count, t.finalized_profit), (1, 1, 7));
        assert!(t.backed_off(1_600).contains(&[4; 32]) && !t.backed_off(1_600).contains(&[1; 32]));
        assert!(t.spent_outpoints().contains(&([1; 32], 0)), "the held one's inputs stay reserved");
        // the kept ones are checked again one interval after their check, not every step
        assert_eq!(t.recheck_pending(&node, 2_100, true).await, Recheck::default());
        node.0.lock().unwrap().extend([held(1), held(2)]);
        assert_eq!(t.recheck_pending(&node, 2_200, true).await.kept, vec![[1; 32], [2; 32]]);
        assert_eq!(t.oldest_pending_daa(2_200), 1_200);
    }

    /// Audit 2026-11-11 M-2: a mempool double spend of the operator's own funding coin holds the coin (out of the funding) and
    /// backs off no order; the hold ends when the node no longer lists the coin, or after `hold_daa`.
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

    /// Audit 2026-11-11 L-5: backoffs that ran out long ago are forgotten.
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
        // accepted again, final 100 DAA later; the unaccepted one stays pending (the node is asked about it, not the view)
        t.observe(&seen(&[([1; 32], "b3", 1_040)]), 1_041);
        let ev = t.observe(&seen(&[([1; 32], "b3", 1_040)]), 1_600);
        assert_eq!(ev.finalized.iter().map(|x| x.txid).collect::<Vec<_>>(), vec![[1; 32]]);
        assert_eq!(t.txs.keys().collect::<Vec<_>>(), vec![&[2; 32]]);
    }

    #[test]
    fn a_double_spend_names_its_outpoint() {
        let id = "ab".repeat(32);
        let want = Some(([0xab; 32], 3));
        // rusty-kaspa's mempool rule error (`TransactionOutpoint` displays as `(<txid>, <index>)`), as the RPC wraps it
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
        // an input no order owns (the operator's funding), an unparsable message, or no message: every order
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
}
