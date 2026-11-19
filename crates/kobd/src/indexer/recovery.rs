//! A follower that moves the store past a gap caused by the node's pruning.
//!
//! The node answers `the queried hash does not have retention root on its chain` when it has
//! pruned the store's cursor. That happens after a downtime longer than the node's retention, or
//! when a store began at the node's pruning point and was too slow, so the node moved the point
//! past the cursor. A plain follower halts at that gap for good ([`Follower::run`],
//! docs/ops/executor.md Part B 7.4).
//!
//! The recovering follower does what an operator does by hand with `kobd db rebase`:
//! - It waits until the node is synced and asks it for the cursor again. A cursor the node knows
//!   again is followed on, with no gap.
//! - It moves the store to the node's pruning point, or to the sink for a store configured to
//!   start there.
//! - It reconciles the tracked orders with the node's UTXO set ([`crate::recover::rebase`]).
//!   Outputs the node no longer has are closed with reason `gap`, and continuations under the
//!   same script are adopted. Nothing the store has is deleted.
//! - It follows from there with a new follower.
//!
//! The move loses what only the pruned chain could tell. An order created inside the skipped
//! stretch and still open is not in the store until its maker imports it (7.5). The move is
//! therefore never silent: it logs a warning and records a [`SkippedGap`] in the health. The gap
//! shows as `gaps` in `GET /v1/health`, in `kob_indexer_gaps_total` and in the status line.
//!
//! A run moves a store at most [`MAX_GAP_REBASES`] times. A store that the node keeps pruning
//! from under it is not catching up, and it stays halted at the gap the status line shows.

use super::follower::{Follower, FollowerConfig};
use super::ingest::Ingest;
use super::status::{FollowerState, HealthState, IndexEvent, SkippedGap};
use crate::config::StartMode;
use crate::rpc::ChainSource;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, watch};

/// The most times one run moves a store past a gap.
pub const MAX_GAP_REBASES: u32 = 10;

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// What a follower is built from. A new follower is built after each move.
pub(super) struct Parts<S: ChainSource> {
    pub source: Arc<S>,
    pub ingest: Arc<Mutex<Ingest>>,
    pub cfg: FollowerConfig,
    pub health: Arc<HealthState>,
    pub events: broadcast::Sender<Arc<IndexEvent>>,
}

/// Follows until `stop`, moving the store past every pruned gap (up to [`MAX_GAP_REBASES`]).
pub(super) async fn follow<S: ChainSource>(p: Parts<S>, mut stop: watch::Receiver<bool>) {
    let mut rebases = 0u32;
    let mut failures = 0u32;
    loop {
        let follower = Follower::new(p.source.clone(), p.ingest.clone(), p.cfg.clone(), p.health.clone(), p.events.clone());
        let Some(reason) = follower.run_recovering(stop.clone()).await else { return };
        drop(follower);
        if rebases >= MAX_GAP_REBASES {
            tracing::error!(
                "indexer halted: {reason}. The store was moved past {rebases} gaps in this run and the node keeps pruning it from \
                 under the follower; it is not moved again. See docs/ops/executor.md (Part B, 7.4)."
            );
            let _ = stop.wait_for(|s| *s).await;
            return;
        }
        let moved = match still_disowned(&p).await {
            Ok(true) => move_past_gap(&p, &reason, rebases + 1).await.map(|()| true),
            Ok(false) => Ok(false),
            Err(e) => Err(e),
        };
        match moved {
            Ok(true) => {
                rebases += 1;
                failures = 0;
            }
            Ok(false) => failures = 0,
            Err(e) => {
                // The node cannot be asked now: it is not synced, has no UTXO index, or moved
                // its pruning point again. The next look is later, and a failed move does not
                // count against the bound.
                failures = failures.saturating_add(1);
                tracing::warn!(error = %e, attempt = failures, "the store cannot be moved past the pruned gap yet; trying again");
                p.health.update(|h| h.last_error = Some(format!("moving the store past the gap: {e}")));
                let wait = p.cfg.backoff.saturating_mul(1u32 << failures.min(10).saturating_sub(1)).min(p.cfg.max_backoff);
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = async { let _ = stop.wait_for(|s| *s).await; } => return,
                }
            }
        }
    }
}

/// Whether the synced node still disowns the store's cursor. A node that is not synced may not
/// know it yet: the store is not moved until it is (an error, tried again later). A cursor the
/// node knows again is followed on, with no gap.
async fn still_disowned<S: ChainSource>(p: &Parts<S>) -> Result<bool, crate::recover::RecoverError> {
    use crate::rpc::NodeErrorKind::{RetentionRootMissing, UnknownHash};
    let info = p.source.server_info().await?;
    if !info.is_synced {
        let why = format!("{}: the store is not moved past the gap until it is", super::follower::NOT_SYNCED);
        p.health.update(|h| h.state = FollowerState::NodeUnavailable);
        return Err(crate::recover::RecoverError::Invalid(why));
    }
    let Some(c) = p.ingest.lock().unwrap_or_else(|e| e.into_inner()).cursor()? else { return Ok(true) };
    match p.source.chain_hashes(c.hash).await {
        Ok(_) => {
            tracing::info!(cursor = %c.hash, "the synced node knows the cursor again; following on, no gap");
            p.health.update(|h| {
                h.state = FollowerState::Starting;
                h.gap_reason = None;
                h.last_error = None;
            });
            Ok(false)
        }
        Err(e) if matches!(e.kind(), UnknownHash | RetentionRootMissing) => Ok(true),
        Err(e) => Err(e.into()),
    }
}

/// Moves the store to the node's pruning point and records the skipped stretch.
async fn move_past_gap<S: ChainSource>(p: &Parts<S>, reason: &str, attempt: u32) -> Result<(), crate::recover::RecoverError> {
    let to = match p.cfg.start {
        StartMode::Sink => StartMode::Sink,
        _ => StartMode::PruningPoint,
    };
    let from = p.ingest.lock().unwrap_or_else(|e| e.into_inner()).cursor()?;
    tracing::warn!(
        attempt,
        "the node pruned the history the store was following ({reason}); moving the store to the node's {} and following from there",
        if to == StartMode::Sink { "sink" } else { "pruning point" }
    );
    let r = crate::recover::rebase(&p.ingest, p.source.as_ref(), &p.cfg.network, &to, true).await?;
    let rep = r.reconciled.unwrap_or_default();
    let gap = SkippedGap {
        from_daa: from.map_or(r.cursor.daa, |c| c.daa),
        to_daa: r.cursor.daa,
        to_hash: r.cursor.hash,
        at_unix: now_unix(),
        orders_closed: rep.closed,
        orders_adopted: rep.adopted,
    };
    tracing::warn!(
        from_daa = gap.from_daa,
        to_daa = gap.to_daa,
        skipped_secs = gap.daa() / 10,
        cursor = %r.cursor.hash,
        orders_closed = gap.orders_closed,
        orders_adopted = gap.orders_adopted,
        "GAP skipped: the chain between the store's cursor and the node's pruning point is gone from the node; orders created in \
         it and still open are not in the store until their makers import them (docs/ops/executor.md, Part B 7.4/7.5)"
    );
    p.health.update(|h| {
        h.gaps.push(gap);
        h.book_complete = false;
        h.history_from_daa = Some(h.history_from_daa.map_or(r.cursor.daa, |d| d.max(r.cursor.daa)));
        h.state = FollowerState::Starting;
        h.gap_reason = None;
        h.last_error = None;
        h.cursor_hash = Some(r.cursor.hash);
        h.cursor_daa = r.cursor.daa;
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::follower::NOT_SYNCED;
    use crate::testkit::{Harness, MockNode};

    fn parts(hs: &Harness) -> Parts<MockNode> {
        let cfg = FollowerConfig::from_indexer(&crate::config::IndexerConfig { network: "testnet-10".into(), ..Default::default() });
        Parts { source: hs.node.clone(), ingest: hs.ingest.clone(), cfg, health: hs.health.clone(), events: hs.events.clone() }
    }

    /// The store is moved past a gap only once the node is synced and, asked again, still
    /// disowns the cursor.
    #[tokio::test]
    async fn the_store_is_moved_only_when_the_synced_node_still_disowns_the_cursor() {
        let hs = Harness::new(crate::indexer::db::open_memory("testnet-10").unwrap(), None);
        hs.node.push_empty(3);
        hs.sync().await;
        let p = parts(&hs);
        let cursor = hs.node.tip();
        hs.node.forget(cursor);
        hs.node.set_synced(false);
        let e = still_disowned(&p).await.unwrap_err();
        assert!(e.to_string().contains(NOT_SYNCED), "{e}");
        assert_eq!(hs.health.snapshot().state, FollowerState::NodeUnavailable);
        hs.node.set_synced(true);
        assert!(still_disowned(&p).await.unwrap(), "synced and still unknown: moved");
        hs.node.remember(cursor);
        assert!(!still_disowned(&p).await.unwrap(), "known again: not moved");
        let h = hs.health.snapshot();
        assert!(h.gaps.is_empty() && h.gap_reason.is_none(), "{h:?}");
        assert_eq!(h.state, FollowerState::Starting);
    }
}
