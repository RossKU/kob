//! A follower that gets past a gap the node's pruning caused.
//!
//! A store whose cursor the node has pruned (a downtime longer than the node's retention, or a store that began at the
//! node's pruning point and was slow enough for the node to move it past the cursor) gets the node's answer `the queried
//! hash does not have retention root on its chain`, and a plain follower halts at that gap for good ([`Follower::run`],
//! docs/ops/executor.md Part B 7.4). The recovering follower does what the operator would do by hand (`index rebase`) and
//! carries on: it moves the store to the node's pruning point (to the sink for a store configured to start there), with the
//! tracked orders reconciled with the node's UTXO set ([`crate::recover::rebase`]: outputs the node no longer has are closed
//! with reason `gap`, continuations under the same script are adopted; nothing the store has is deleted), and follows from
//! there with a new follower.
//!
//! The move loses what only the pruned chain could tell: an order created inside the skipped stretch and still open is not in
//! the store until its maker imports it (7.5). So it is never silent: the warning in the log, [`SkippedGap`] in the health
//! (`gaps` in `GET /v1/health`, `kob_indexer_gaps_total`, the status line).
//!
//! A run moves a store at most [`MAX_GAP_REBASES`] times; a store the node keeps pruning from under it is not catching up, and
//! stays halted at the gap the status line shows.

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

/// What a follower is built from (a new one follows each move).
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
        match move_past_gap(&p, &reason, rebases + 1).await {
            Ok(()) => {
                rebases += 1;
                failures = 0;
            }
            Err(e) => {
                // the node cannot be asked now (not synced, no UTXO index, the pruning point moved again): the next look is
                // later, and a failed move does not count against the bound
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
        h.state = FollowerState::Starting;
        h.gap_reason = None;
        h.last_error = None;
        h.cursor_hash = Some(r.cursor.hash);
        h.cursor_daa = r.cursor.daa;
    });
    Ok(())
}
