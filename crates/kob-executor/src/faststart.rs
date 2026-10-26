//! Fast start: `kob-executor run` on an EMPTY data directory trades within seconds and fills in the history afterwards.
//!
//! A normal first start indexes the chain from the node's pruning point (about 30 hours on testnet-10; on a phone over public
//! nodes that takes hours) and the matcher plans nothing until the store has caught up. With `fast_start` the process runs
//! TWO standard indexers, each with its own database (two histories are never merged into one):
//!
//! * the tip store, `<data_dir>/tip/`, whose cursor starts at the node's current sink (`start = sink`, no record log). It
//!   catches up in seconds, and the matcher, the keepers and the read API work on it at once;
//! * the full store, `<data_dir>/`, exactly what a start without fast start builds: from the pruning point, with the record
//!   log, and no matcher.
//!
//! The executor cannot lose by trading on the tip store. It only acts on orders the node confirms, and the covenants enforce
//! every order's terms; the store merely misses the orders created before it started following (and the operator's older
//! token holdings) until the full store has them. While it runs, `/v1/health` reports `book_complete: false` and the full
//! store's progress (`backfill`), and the console status line says when the full book will be there.
//!
//! When the full store is caught up (the rule the matcher applies to any store: [`HealthSnapshot::not_ready`]) the book source
//! switches to it ([`FastSource`], between two ticks of the runner, so the matcher and the keepers never plan against both),
//! the read API restarts over it, and `<data_dir>/tip/` is deleted. A later start finds only the full store and uses it
//! directly. A start that finds a leftover tip store (the process stopped before the switch) resumes the fast start: a tip
//! store more than ten minutes behind the node is moved to the sink first (`index rebase --to sink`, which also closes the
//! orders the node no longer holds).
//!
//! A node that prunes its oldest data while the full store is still near its start (the pruning point it began at moves past
//! its cursor; the node then answers `the queried hash does not have retention root on its chain`) would leave the full store
//! halted at a gap for good: its follower moves it to the node's new pruning point instead (a few times at most,
//! [`crate::indexer::recovery`], as for every store of a normal start).
//!
//! The tip follower does not share connections with the backfill. It has a node client of its own (one request at a time,
//! the small-call connection), so a window of the backfill that takes half a minute never holds a tip request back; the
//! backfill is the usual parallel fetch on the usual client.
//!
//! [`HealthSnapshot::not_ready`]: crate::indexer::status::HealthSnapshot::not_ready

use crate::config::{IndexerConfig, StartMode};
use crate::executor::IndexerSource;
use crate::indexer::status::{Backfill, BackfillPace, HealthState};
use crate::indexer::{self, ApiTask, Indexer, Running};
use crate::matcher::book::{ListedOrder, MemoryBook};
use crate::matcher::node::NodeApi;
use crate::matcher::run::{BookRead, BookSource, RunConfig, Runner};
use crate::matcher::wallet::Signer;
use crate::rpc::ChainSource;
use anyhow::Result;
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{oneshot, watch};

/// The tip store's directory inside the data directory.
pub const TIP_DIR: &str = "tip";

/// A tip store this many DAA (ten minutes) behind the node at a restart is moved to the sink instead of followed from where it
/// stopped: the follow would take longer than the fast start is worth.
pub const STALE_TIP_DAA: u64 = 6_000;

/// How often the backfill progress is measured and published.
const PROGRESS_EVERY: Duration = Duration::from_secs(2);

/// What a start of `kob-executor run` does about fast start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// One store, as before: fast start is off, the data directory has a store already, or the start is not the pruning point.
    Normal,
    /// An empty data directory: create the tip and the full store.
    Fresh,
    /// A tip store is left over from a start that never reached the switch: use it again.
    Resume,
}

/// The tip store's directory for `cfg`.
pub fn tip_dir(cfg: &IndexerConfig) -> PathBuf {
    cfg.data_dir.join(TIP_DIR)
}

/// What a start with `cfg` does (decided before any store is opened).
pub fn plan(cfg: &IndexerConfig) -> Plan {
    if !cfg.fast_start || cfg.start != StartMode::PruningPoint {
        Plan::Normal
    } else if tip_dir(cfg).exists() {
        Plan::Resume
    } else if !cfg.db_path().exists() {
        Plan::Fresh
    } else {
        Plan::Normal
    }
}

/// The configuration of the tip store: its own directory, the node's sink as the start, no record log (it is a scratch
/// store, rebuilt in seconds), one request at a time to each node (see the module documentation).
pub fn tip_config(cfg: &IndexerConfig) -> IndexerConfig {
    let mut t = cfg.clone();
    t.data_dir = tip_dir(cfg);
    t.start = StartMode::Sink;
    t.fast_start = false;
    t.records.enabled = false;
    t.records.dir = None;
    t.fetch_parallel = 1;
    t.launch = None;
    t
}

/// The book source of a fast start: the tip store until the full store has caught up, then the full store for good.
pub struct FastSource {
    tip: Option<IndexerSource>,
    full: IndexerSource,
    tip_health: Arc<HealthState>,
    /// The two stores agreed on the acceptance of every transaction the runner tracks at its last look.
    agree: bool,
    handover: Option<oneshot::Sender<()>>,
}

impl FastSource {
    /// `tip_health`: where the status line reads the backfill's progress. Both sources carry the health that gates them
    /// (`IndexerSource::with_health`).
    pub fn new(tip: IndexerSource, tip_health: Arc<HealthState>, full: IndexerSource) -> (FastSource, oneshot::Receiver<()>) {
        let (tx, rx) = oneshot::channel();
        (FastSource { tip: Some(tip), full, tip_health, agree: false, handover: Some(tx) }, rx)
    }

    /// Still on the tip store.
    pub fn on_tip(&self) -> bool {
        self.tip.is_some()
    }

    fn active(&mut self) -> &mut IndexerSource {
        match self.tip.as_mut() {
            Some(t) => t,
            None => &mut self.full,
        }
    }

    fn hand_over(&mut self) {
        // the tip store's handles are dropped here: the book, the watches and the reads all come from the full store from the
        // next call on, and the coordinator deletes the tip store once it has stopped its follower and read API
        self.tip = None;
        tracing::info!("fast start: the full book has caught up; the matcher, the keepers and the read API switch to it");
        if let Some(tx) = self.handover.take() {
            let _ = tx.send(());
        }
    }
}

impl BookSource for FastSource {
    fn ready(&mut self) -> Result<(), String> {
        // read once per call: a stale "agree" never carries over to the next tick
        let agree = std::mem::replace(&mut self.agree, false);
        if self.tip.is_some() && agree && self.full.ready().is_ok() {
            self.hand_over();
        }
        self.active().ready()
    }

    fn snapshot(&mut self, operator: &[u8; 32]) -> anyhow::Result<MemoryBook> {
        self.active().snapshot(operator)
    }

    fn snapshot_read(&mut self, operator: &[u8; 32]) -> Option<BookRead> {
        self.active().snapshot_read(operator)
    }

    fn watch(&mut self, txid: [u8; 32]) {
        // both: the full store must have seen a transaction's acceptance by the time it takes over
        if let Some(t) = self.tip.as_mut() {
            t.watch(txid);
        }
        self.full.watch(txid);
    }

    fn acceptance(&mut self, tracked: &[[u8; 32]]) -> Option<BTreeMap<[u8; 32], (String, u64)>> {
        let full = self.full.acceptance(tracked);
        match self.tip.as_mut() {
            None => full,
            Some(tip) => {
                let seen = tip.acceptance(tracked);
                // the switch waits until the full store has applied the chain block that accepted each transaction the tip
                // store saw accepted (it is behind, never ahead, so this holds once it is within a few blocks of the tip)
                self.agree = match (&seen, &full) {
                    (Some(t), Some(f)) => t.iter().all(|(id, acc)| f.get(id) == Some(acc)),
                    _ => false,
                };
                seen
            }
        }
    }

    fn keeper_only_orders(&mut self) -> Vec<ListedOrder> {
        self.active().keeper_only_orders()
    }

    fn operator_tokens(&mut self, operator: &[u8; 32]) -> Vec<crate::maintenance::OwnToken> {
        self.active().operator_tokens(operator)
    }

    fn report_frozen(&mut self, daa: u64, flagged: &[([u8; 32], String)], cleared: &[[u8; 32]]) {
        self.active().report_frozen(daa, flagged, cleared)
    }

    fn metrics(&mut self) -> String {
        self.active().metrics()
    }

    fn status(&mut self) -> Option<String> {
        let own = self.active().status();
        if self.tip.is_none() {
            return own;
        }
        let fast = match self.tip_health.snapshot().backfill {
            Some(b) => b.status_text(),
            None => "fast start: trading on the orders seen since the start; full book: estimating".to_string(),
        };
        Some(match own {
            Some(o) => format!("{fast} | {o}"),
            None => fast,
        })
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// When the tip store was created (a small file in its directory; written on the first start, read on a resume).
fn tip_since(dir: &Path) -> u64 {
    let f = dir.join("since");
    if let Some(t) = std::fs::read_to_string(&f).ok().and_then(|s| s.trim().parse::<u64>().ok()) {
        return t;
    }
    let now = unix_now();
    let _ = std::fs::write(&f, now.to_string());
    now
}

/// Deletes `dir` (the tip store), a few times over: on some systems a file closed a moment ago is still held.
async fn remove_dir(dir: &Path) {
    for attempt in 1..=10 {
        match tokio::fs::remove_dir_all(dir).await {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) if attempt == 10 => {
                tracing::warn!(dir = %dir.display(), error = %e, "cannot delete the tip store; it is removed at the next start");
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
}

/// Opens the tip store. A leftover one (`Plan::Resume`) that is far behind the node is moved to the sink; one that cannot be
/// opened or moved is deleted and started afresh.
async fn open_tip<C: ChainSource>(tip_cfg: &IndexerConfig, plan: Plan, chain: &C) -> Result<Indexer> {
    if plan == Plan::Resume {
        match Indexer::open(tip_cfg.clone()) {
            Ok(idx) => {
                if tip_is_current(&idx, chain, &tip_cfg.network).await {
                    return Ok(idx);
                }
                drop(idx);
            }
            Err(e) => tracing::warn!(error = %e, "the tip store of the last start cannot be opened"),
        }
        tracing::warn!("fast start: starting the tip store afresh at the node's sink");
        remove_dir(&tip_cfg.data_dir).await;
    }
    Ok(Indexer::open(tip_cfg.clone())?)
}

/// Whether a leftover tip store can go on: within [`STALE_TIP_DAA`] of the node, or moved to the sink now. `false`: it could not
/// be moved.
async fn tip_is_current<C: ChainSource>(idx: &Indexer, chain: &C, network: &str) -> bool {
    let cursor = idx.ingest.lock().unwrap_or_else(|e| e.into_inner()).cursor();
    let Some(cursor) = cursor.ok().flatten().filter(|c| c.daa > 0) else { return true };
    let node_daa = match chain.dag_info().await {
        Ok(d) => d.virtual_daa_score,
        // no answer: the follower retries as it always does
        Err(_) => return true,
    };
    if node_daa.saturating_sub(cursor.daa) <= STALE_TIP_DAA {
        return true;
    }
    match crate::recover::rebase(&idx.ingest, chain, network, &StartMode::Sink, true).await {
        Ok(r) => {
            tracing::info!(cursor = %r.cursor.hash, "fast start: the tip store was far behind the node; moved to the sink");
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "fast start: the tip store cannot be moved to the sink");
            false
        }
    }
}

/// What the coordinator owns while the executor runs.
struct Stores {
    tip: Indexer,
    tip_run: Running,
    tip_api: Option<ApiTask>,
    full: Indexer,
    full_run: Running,
    since: u64,
    daa_per_second: u64,
}

/// Publishes the full store's progress in the tip store's health (what `/v1/health` and the status line report), until the
/// full store has caught up and the book source switches; then moves the read API and deletes the tip store. Returns the
/// full store's tasks, which keep running until `stop`.
async fn coordinate(s: Stores, handover: oneshot::Receiver<()>, mut stop: watch::Receiver<bool>) {
    let started = std::time::Instant::now();
    let mut pace = BackfillPace::default();
    let mut tick = tokio::time::interval(PROGRESS_EVERY);
    let mut handover = handover;
    let switched = loop {
        tokio::select! {
            _ = tick.tick() => publish_progress(&s, &mut pace, started),
            r = &mut handover => break r.is_ok(),
            _ = async { let _ = stop.wait_for(|x| *x).await; } => break false,
        }
    };
    let Stores { tip, tip_run, tip_api, full, full_run, .. } = s;
    // the tip store goes in either case: its follower and API stop first
    if let Some(api) = tip_api {
        api.stop().await;
    }
    tip_run.stop().await;
    let tip_dir = tip.cfg.data_dir.clone();
    drop(tip);
    if !switched {
        // stopped by the shutdown before the switch: the tip store stays for the next start to resume
        full_run.stop().await;
        return;
    }
    remove_dir(&tip_dir).await;
    let full_api = if full.cfg.api.enabled {
        match full.spawn_api() {
            Ok(a) => Some(a),
            Err(e) => {
                tracing::error!("the read API cannot start over the full store: {e}");
                None
            }
        }
    } else {
        None
    };
    tracing::info!("fast start finished: the tip store is deleted; the executor runs on the full store");
    let _ = stop.wait_for(|x| *x).await;
    if let Some(a) = full_api {
        a.stop().await;
    }
    full_run.stop().await;
}

fn publish_progress(s: &Stores, pace: &mut BackfillPace, started: std::time::Instant) {
    let h = s.full.health.snapshot();
    // a store that has applied nothing yet has no position (cursor DAA 0): the lag would be the node's whole history
    let lag = h.lag_daa().filter(|_| h.cursor_daa > 0);
    let (rate, eta) = match lag {
        Some(l) => pace.push(started.elapsed().as_secs_f64(), l, h.cursor_daa),
        None => (None, None),
    };
    let b = Backfill {
        state: h.state,
        cursor_daa: h.cursor_daa,
        lag_daa: lag,
        lag_seconds: lag.map(|l| l / s.daa_per_second.max(1)),
        rate_daa_per_sec: rate,
        eta_seconds: eta,
        tip_since_unix: s.since,
        gaps: h.gaps.clone(),
    };
    s.tip.health.update(|t| {
        t.book_complete = false;
        t.backfill = Some(b);
    });
}

/// `kob-executor run` with a fast start ([`Plan::Fresh`] or [`Plan::Resume`]): the tip and the full store follow `tip_chain` and
/// `full_chain` (two node clients: see the module documentation), the runner works on [`FastSource`] until `shutdown`.
pub async fn run_with<C, N, S>(
    cfg: IndexerConfig,
    plan: Plan,
    full_chain: Arc<C>,
    tip_chain: Arc<C>,
    node: N,
    signer: Box<dyn Signer>,
    run_cfg: RunConfig,
    shutdown: S,
) -> Result<()>
where
    C: ChainSource,
    N: NodeApi,
    S: Future<Output = ()>,
{
    debug_assert!(plan != Plan::Normal, "a normal start has no fast start");
    let tip_cfg = tip_config(&cfg);
    let tip = open_tip(&tip_cfg, plan, tip_chain.as_ref()).await?;
    let since = tip_since(&tip_cfg.data_dir);
    tip.health.update(|h| h.book_complete = false);
    let full = Indexer::open(cfg.clone())?;
    tracing::info!(
        tip = %tip_cfg.data_dir.display(),
        full = %cfg.data_dir.display(),
        "fast start: trading on a store that follows the chain from the node's sink while the full book is indexed"
    );

    let tip_reader = indexer::db::open_reader(&tip_cfg.db_path(), &tip_cfg.network)?;
    let full_reader = indexer::db::open_reader(&cfg.db_path(), &cfg.network)?;
    let tip_source = IndexerSource::new(tip.ingest.clone()).with_health(tip.health.clone()).with_reader(tip_reader);
    let full_source = IndexerSource::new(full.ingest.clone()).with_health(full.health.clone()).with_reader(full_reader);
    let (source, handover) = FastSource::new(tip_source, tip.health.clone(), full_source);

    let tip_api = if cfg.api.enabled { Some(tip.spawn_api()?) } else { None };
    let tip_run = tip.spawn_recovering(tip_chain);
    // the full store's follower moves it past a gap the node's pruning causes (a start at the pruning point is hit by it)
    let full_run = full.spawn_recovering(full_chain);
    let (stop_tx, stop_rx) = watch::channel(false);
    let stores = Stores { tip, tip_run, tip_api, full, full_run, since, daa_per_second: cfg.daa_per_second };
    let coordinator = tokio::spawn(coordinate(stores, handover, stop_rx));

    let mut runner = Runner::new(node, source, signer, run_cfg);
    let res = runner.run(shutdown).await;
    tracing::info!("shutting down");
    let _ = stop_tx.send(true);
    let _ = coordinator.await;
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::MockNode;

    /// A tip store far behind the node is moved to the sink on a resume (its tracked orders reconciled); one close behind is
    /// left to follow.
    #[tokio::test]
    async fn a_leftover_tip_store_is_moved_to_the_sink_only_when_far_behind() {
        let dir = tempfile::tempdir().unwrap();
        let node = MockNode::new("testnet-10");
        let tokens = dir.path().join("tokens.json");
        std::fs::write(&tokens, r#"[]"#).unwrap();
        let cfg = IndexerConfig {
            network: "testnet-10".into(),
            data_dir: dir.path().join("tip"),
            tokens_path: Some(tokens),
            start: StartMode::Hash(node.anchor()),
            ..IndexerConfig::default()
        };
        let idx = Indexer::open(cfg).unwrap();
        let follower = idx.follower(node.clone());
        node.push_empty(10);
        while !matches!(follower.step().await, crate::indexer::follower::StepOutcome::Idle) {}
        let cursor = |i: &Indexer| i.ingest.lock().unwrap().cursor().unwrap().unwrap();
        assert_eq!(cursor(&idx).hash, node.tip());

        node.push_empty(STALE_TIP_DAA as usize - 1);
        assert!(tip_is_current(&idx, node.as_ref(), "testnet-10").await);
        assert_ne!(cursor(&idx).hash, node.tip(), "within ten minutes: left to follow");

        node.push_empty(2);
        assert!(tip_is_current(&idx, node.as_ref(), "testnet-10").await);
        assert_eq!(cursor(&idx).hash, node.tip(), "far behind: moved to the sink");
    }
}
