//! Submission, mempool conflicts, acceptance tracking and rollback (`docs/spec/matcher.md` §1.3,
//! §7, §8).
//!
//! * Every transaction is engine-validated before it reaches [`Tracker::submit`].
//! * `RejectDoubleSpendInMempool` is benign (a competing matcher, a cancel, a refund): the order
//!   whose input the node names backs off exponentially (every order of the transaction when the
//!   input is no order's, [`Tracked::owners`]); nothing is ever replaced blindly (the matcher never
//!   calls `submitTransactionReplacement`).
//! * A missing input (an orphan: the input's spend is already in a block the book has not shown
//!   yet) names no outpoint: the tracker asks the node which of the order inputs are still unspent
//!   ([`Tracked::addresses`]) and backs off the orders whose inputs are gone; the other orders of
//!   the transaction are planned again at once (every order backs off only when the node cannot
//!   tell).
//! * Acceptance comes from `getVirtualChainFromBlockV2` (`Low`): a transaction is *accepted* when a
//!   chain block accepts it and *final* (done) after `final_depth` more DAA; a `removed` chain
//!   block moves its transactions back to pending (reorg), where they are re-checked.
//! * While a transaction is pending or accepted-but-not-final, the outpoints it spends are
//!   excluded from planning, so no order is planned twice; a pending transaction that is neither
//!   accepted nor still possible within `pending_timeout` DAA is dropped and its orders back off.
//!   A pending transaction one of whose order inputs the book no longer lists on two steps in a row
//!   and the node confirms spent (another transaction spent it: the race is lost) is dropped at once
//!   ([`Tracker::drop_lost`]): the order whose input is gone backs off, the others are planned again.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::book::{CovId, Outpoint};
use super::node::{classify_submit_error, ChainUpdate, NodeApi, RpcError, SubmitOutcome};

/// Tracker settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackerConfig {
    /// DAA after acceptance at which a transaction is final (default 100, §1.3).
    pub final_depth: u64,
    /// DAA after which an unaccepted transaction is given up (default 600).
    pub pending_timeout: u64,
    /// First back-off of an order after a conflict (DAA; doubles per attempt).
    pub backoff_base: u64,
    /// Longest back-off (DAA).
    pub backoff_max: u64,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        TrackerConfig { final_depth: 100, pending_timeout: 600, backoff_base: 20, backoff_max: 600 }
    }
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
    pub dropped: Vec<[u8; 32]>,
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
}

impl Tracker {
    pub fn new(cfg: TrackerConfig) -> Self {
        Tracker { cfg, ..Default::default() }
    }

    /// Outpoints no plan may spend (pending and accepted-but-not-final transactions).
    pub fn spent_outpoints(&self) -> BTreeSet<Outpoint> {
        self.txs.values().flat_map(|t| t.spends.iter().copied()).collect()
    }

    /// Order ids backed off at `daa`.
    pub fn backed_off(&self, daa: u64) -> BTreeSet<CovId> {
        self.backoff.iter().filter(|(_, (until, _))| *until > daa).map(|(k, _)| *k).collect()
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
                for id in &t.orders {
                    self.backoff.remove(id);
                }
                self.txs.insert(t.txid, t);
            }
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
                self.back_off(&t.orders, daa);
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
                SubmitOutcome::Accepted | SubmitOutcome::AlreadyKnown => {
                    if let Some(t) = self.txs.get_mut(&id) {
                        t.unconfirmed = false;
                    }
                    self.submitted += 1;
                }
                SubmitOutcome::Unknown | SubmitOutcome::Busy => {}
                SubmitOutcome::DoubleSpend | SubmitOutcome::MissingInput | SubmitOutcome::Rejected => {
                    if let Some(t) = self.txs.remove(&id) {
                        if outcome == SubmitOutcome::Rejected {
                            self.rejected += 1;
                        } else {
                            self.conflicts += 1;
                        }
                        let ids = if outcome == SubmitOutcome::Rejected {
                            t.orders.clone()
                        } else {
                            lost_orders(node, &t, outcome, msg.as_deref()).await
                        };
                        self.back_off(&ids, daa);
                    }
                }
            }
            out.push((id, outcome));
        }
        out
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
                self.conflicts += 1;
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

    /// Final: accepted + depth. Given up: never accepted within the timeout (a lost race).
    fn settle(&mut self, daa: u64, mut ev: TrackerEvents) -> TrackerEvents {
        for t in self.txs.values_mut() {
            if t.accepted.is_some() {
                t.unconfirmed = false;
            }
        }
        let mut done = vec![];
        for (k, t) in &self.txs {
            match &t.accepted {
                Some((_, d)) if daa >= d + self.cfg.final_depth => done.push((*k, true)),
                None if daa >= t.submitted_daa + self.cfg.pending_timeout => done.push((*k, false)),
                _ => {}
            }
        }
        for (k, fin) in done {
            let t = self.txs.remove(&k).expect("listed");
            if fin {
                self.finalized_count += 1;
                self.finalized_profit += t.profit;
                ev.finalized.push(t);
            } else {
                let ids = t.orders.clone();
                self.back_off(&ids, daa);
                ev.dropped.push(k);
            }
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
        // Re-accepted, then final after 100 DAA; the other one times out and backs off.
        let up =
            ChainUpdate { removed: vec![], added: vec![ChainBlock { hash: "b2".into(), daa_score: 1_030, accepted: vec![[1; 32]] }] };
        t.apply(&up, 1_031);
        let ev = t.apply(&ChainUpdate::default(), 1_600);
        assert_eq!(ev.dropped, vec![[2; 32]]);
        assert!(t.backed_off(1_600).contains(&[2; 32]));
        assert_eq!(ev.finalized.iter().map(|x| x.txid).collect::<Vec<_>>(), vec![[1; 32]], "accepted at 1030, final from 1130");
        assert_eq!((t.finalized_count, t.finalized_profit), (1, 7));
        assert!(t.txs.is_empty());
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
        // accepted again, final 100 DAA later; the unaccepted one is dropped at the timeout
        t.observe(&seen(&[([1; 32], "b3", 1_040)]), 1_041);
        let ev = t.observe(&seen(&[([1; 32], "b3", 1_040)]), 1_600);
        assert_eq!(ev.finalized.iter().map(|x| x.txid).collect::<Vec<_>>(), vec![[1; 32]]);
        assert_eq!(ev.dropped, vec![[2; 32]]);
        assert!(t.txs.is_empty());
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
