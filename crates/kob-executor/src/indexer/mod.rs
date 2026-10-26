//! The KOB indexer: follows the node's selected chain, tracks KOB orders (and the token holdings they trade), and
//! keeps a reorg-safe database that the read API serves.
//!
//! Data flow: `rpc` (VSPC v2, High) -> [`follower`] -> [`ingest`] (one atomic SQLite commit per
//! batch; the extracted KOB records are appended to the permanent record log first) ->
//! [`processor`] (order/token state machine over [`record::TxRecord`]s) -> `reads`
//! (queries) -> `api` (REST + WebSocket). Raw node responses are never stored.

pub mod book;
pub mod db;
pub mod flags;
pub mod follower;
pub mod ingest;
pub mod kcc23;
pub mod lean;
pub mod lock;
pub mod market;
pub mod pairs;
pub mod prefetch;
pub mod processor;
pub mod reads;
pub mod record;
pub mod recordlog;
pub mod recovery;
pub mod status;
pub mod trust;

use crate::api::{self, ApiState};
use crate::config::IndexerConfig;
use crate::rpc::ChainSource;
use crate::tokens::TokenAllowlist;
use follower::{Follower, FollowerConfig};
use ingest::{Ingest, IngestConfig};
use processor::Processor;
use recordlog::RecordLog;
use status::{HealthState, IndexEvent};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, watch};

/// The store's data generation: bumped after every change a read can see (a committed batch, revert or operator import,
/// `Ingest`; a possibly-frozen flag, `executor::IndexerSource`). Two reads at one generation read the same rows, which is
/// what the API's response cache keys on (`api::cache`).
#[derive(Clone, Debug, Default)]
pub struct DataGeneration(Arc<std::sync::atomic::AtomicU64>);

impl DataGeneration {
    pub fn get(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }

    /// After a change is committed (never before: a read that saw the old generation must not see older rows than it).
    pub fn bump(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IndexerError {
    #[error(transparent)]
    Db(#[from] db::DbError),
    #[error(transparent)]
    RecordLog(#[from] recordlog::RecordLogError),
    #[error(transparent)]
    Ingest(#[from] ingest::IngestError),
    #[error("tokens: {0}")]
    Tokens(#[from] crate::tokens::TokenLoadError),
    #[error("{0}")]
    Config(String),
    #[error(transparent)]
    Locked(#[from] lock::LockError),
}

/// Everything a running indexer shares between the follower and the API.
pub struct Indexer {
    pub cfg: IndexerConfig,
    pub ingest: Arc<Mutex<Ingest>>,
    pub health: Arc<HealthState>,
    pub events: broadcast::Sender<Arc<IndexEvent>>,
    pub tokens: Arc<TokenAllowlist>,
    /// The data directory's single-writer lock, held as long as the indexer lives ([`lock`]).
    _writer: lock::WriterLock,
}

/// The allowlist of `--tokens`; without one, a deployment build that pins a registry (`deploy-mainnet`) uses the registry it
/// embeds (the pinned one), any other build lists nothing. A loaded file whose hash differs from the pin is allowed (operators
/// may run their own list) and logged; `GET /v1/tokens` reports the hash either way.
pub fn load_tokens(cfg: &IndexerConfig) -> Result<Arc<TokenAllowlist>, IndexerError> {
    let pin = kob_protocol::artifacts::deployment_registry_sha256();
    let t = match (&cfg.tokens_path, pin) {
        (Some(p), _) => TokenAllowlist::load(p)?,
        (None, Some(_)) => {
            tracing::info!("no --tokens: using the registry this deployment build pins (embedded registry/tokens.json)");
            TokenAllowlist::embedded_default()?
        }
        (None, None) => {
            tracing::warn!("no token allowlist configured: with require_allowlist=true nothing will be listed");
            TokenAllowlist::default()
        }
    };
    if let (Some(pin), Some(info)) = (pin, t.info()) {
        if info.sha256 != pin {
            tracing::warn!(
                loaded = %info.sha256,
                pinned = pin,
                source = %info.source,
                "the token registry differs from the one this release pins (an operator list): its tokens are not the release's"
            );
        }
    }
    tracing::info!(tokens = t.len(), "token allowlist loaded");
    Ok(Arc::new(t))
}

fn ingest_config(cfg: &IndexerConfig) -> IngestConfig {
    IngestConfig {
        reorg_window_daa: cfg.reorg_window_hours.saturating_mul(3600).saturating_mul(cfg.daa_per_second),
        checkpoint_daa: cfg.checkpoint_secs.saturating_mul(cfg.daa_per_second),
        declaration_window_daa: cfg.declaration_window_hours.saturating_mul(3600).saturating_mul(cfg.daa_per_second),
        verify_threads: ingest::verify_threads(cfg.verify_threads),
        lean: cfg.lean.then(|| lean::LeanConfig::new(cfg.lean_retention_daa(), cfg.lean_max_db_bytes())),
    }
}

fn processor(cfg: &IndexerConfig, tokens: Arc<TokenAllowlist>) -> Processor {
    Processor { tokens, rules: cfg.rules.clone() }
}

impl Indexer {
    /// Open the database and record log, verify they agree, and build the write path.
    pub fn open(mut cfg: IndexerConfig) -> Result<Indexer, IndexerError> {
        if cfg.lean {
            // lean storage keeps no record log
            cfg.records.enabled = false;
        }
        let writer = lock::WriterLock::acquire(&cfg)?;
        let tokens = load_tokens(&cfg)?;
        let conn = db::open_writer_with(&cfg.db_path(), &cfg.network, cfg.lean)?;
        let mut db_next = ingest::records_next_n(&conn)?;
        let log = if cfg.records.enabled {
            let dir = cfg.records_dir();
            if db_next == 0 && recordlog::record_count(&dir)? > 0 {
                return Err(IndexerError::Config(format!(
                    "record log {} has frames but the database has none: run `kob-executor index replay` to rebuild the database from it, or move the log away to start fresh",
                    dir.display()
                )));
            }
            let (log, report) = match RecordLog::open(&dir, cfg.records.segment_bytes, db_next) {
                // one complete frame past the database: the append an unclean shutdown left uncommitted, or a committed frame
                // a database restored from a backup one frame older lacks. Either way it records the commit this database
                // misses: applied when it continues the database's cursor, refused otherwise (never cut)
                Err(recordlog::RecordLogError::AheadOfDatabase { log_next, db_next: d }) if log_next == d + 1 && d > 0 => {
                    apply_frames_from(&cfg, tokens.clone(), d)?;
                    db_next = ingest::records_next_n(&conn)?;
                    tracing::warn!(frame = d, "applied the one record-log frame the database had not committed");
                    RecordLog::open(&dir, cfg.records.segment_bytes, db_next)?
                }
                r => r?,
            };
            if report.torn_tail_removed {
                tracing::warn!(?report, "repaired the record log after an unclean shutdown");
            }
            Some(log)
        } else {
            None
        };
        let proc = processor(&cfg, tokens.clone());
        let ingest = Ingest::new(conn, proc, log).with_config(ingest_config(&cfg));
        let health = Arc::new(HealthState::new(&cfg.network));
        health.update(|h| {
            h.lag_tolerance_daa = cfg.lag_tolerance_daa();
            h.lean = cfg.lean;
        });
        if let Some(c) = ingest.cursor()? {
            let next = ingest.records_next();
            let orders = ingest.order_count()?;
            health.update(|h| {
                h.cursor_hash = Some(c.hash);
                h.cursor_daa = c.daa;
                h.orders_total = orders;
                h.records_next_n = next.unwrap_or(db_next);
            });
        }
        let (events, _) = broadcast::channel(1024);
        Ok(Indexer { cfg, ingest: Arc::new(Mutex::new(ingest)), health, events, tokens, _writer: writer })
    }

    pub fn follower<S: ChainSource>(&self, source: Arc<S>) -> Follower<S> {
        Follower::new(source, self.ingest.clone(), FollowerConfig::from_indexer(&self.cfg), self.health.clone(), self.events.clone())
    }

    pub fn api_state(&self) -> Result<ApiState, IndexerError> {
        let pool = db::ReadPool::open(&self.cfg.db_path(), self.cfg.api.read_pool_size)?;
        let generation = self.ingest.lock().unwrap_or_else(|e| e.into_inner()).generation().clone();
        let st = ApiState {
            pool,
            generation,
            health: self.health.clone(),
            events: self.events.clone(),
            tokens: self.tokens.clone(),
            cfg: self.cfg.api.clone(),
            settle_depth_daa: self.cfg.settle_depth_daa,
            daa_per_second: self.cfg.daa_per_second,
            lag_alarm_hours: self.cfg.lag_alarm_hours.clone(),
        };
        st.validate().map_err(IndexerError::Config)?;
        if let Some(w) = self.cfg.api.proxy_warning() {
            tracing::warn!("{w}");
        }
        Ok(st)
    }
    /// Follow the node and serve the API until `shutdown` resolves.
    pub async fn run(self, shutdown: impl std::future::Future<Output = ()> + Send + 'static) -> Result<(), IndexerError> {
        let running = self.spawn(crate::index_cli::node_client(&self.cfg))?;
        shutdown.await;
        tracing::info!("shutting down");
        running.stop().await;
        self.flush().await;
        Ok(())
    }

    /// At a clean stop, after the follower and the API are stopped: write the write-ahead log into the database file and cut
    /// it back, so that the file alone is the whole store (a kill leaves the log for the next open to replay, which is safe
    /// too). Bounded: a reader that holds the log open must not hold the stop.
    pub async fn flush(&self) {
        let ingest = self.ingest.clone();
        let task = tokio::task::spawn_blocking(move || {
            let g = ingest.lock().unwrap_or_else(|e| e.into_inner());
            g.conn().query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        });
        match tokio::time::timeout(std::time::Duration::from_secs(3), task).await {
            Ok(Ok(Ok(()))) => tracing::info!(file = %self.cfg.db_path().display(), "database flushed"),
            Ok(Ok(Err(e))) => {
                tracing::warn!(error = %e, "the database could not be flushed at the stop (the next start replays its log)")
            }
            _ => tracing::warn!("the database was not flushed within 3 s of the stop (the next start replays its log)"),
        }
    }

    /// Start the follower and (when enabled) the API server on the current runtime. The indexer
    /// stays usable (`ingest`, `health`) while they run: `kob-executor run` reads the book through
    /// the same `ingest` handle. The follower moves the store past a gap the node's pruning caused instead of halting at it
    /// ([`recovery`]).
    pub fn spawn<S: ChainSource>(&self, source: Arc<S>) -> Result<Running, IndexerError> {
        // validate the API configuration before anything is started
        let api_state = if self.cfg.api.enabled { Some(self.api_state()?) } else { None };
        let mut running = self.spawn_recovering(source);
        if let Some(state) = api_state {
            running.api = Some(serve_task(state, running.stop.subscribe()));
        }
        Ok(running)
    }

    /// Start the follower alone (no API server): the fast start runs the API itself ([`crate::faststart`]), over whichever
    /// of its two stores is current.
    pub fn spawn_follower<S: ChainSource>(&self, source: Arc<S>) -> Running {
        let follower = Arc::new(self.follower(source));
        let (stop_tx, stop_rx) = watch::channel(false);
        let follow = tokio::spawn(async move { follower.run(stop_rx).await });
        Running { stop: stop_tx, follow, api: None }
    }

    /// [`Indexer::spawn_follower`], except that a gap the node's pruning caused does not halt it: the store is moved to the node's
    /// pruning point (to the sink for a store that starts there) and followed from there by a new follower, a bounded number of
    /// times ([`recovery`]; the skipped chain is reported in the log and the health).
    pub fn spawn_recovering<S: ChainSource>(&self, source: Arc<S>) -> Running {
        let parts = recovery::Parts {
            source,
            ingest: self.ingest.clone(),
            cfg: FollowerConfig::from_indexer(&self.cfg),
            health: self.health.clone(),
            events: self.events.clone(),
        };
        let (stop_tx, stop_rx) = watch::channel(false);
        let follow = tokio::spawn(recovery::follow(parts, stop_rx));
        Running { stop: stop_tx, follow, api: None }
    }

    /// Start the read API server over this indexer's store (`api_state`), whatever `api.enabled` says.
    pub fn spawn_api(&self) -> Result<ApiTask, IndexerError> {
        let state = self.api_state()?;
        let (stop, rx) = watch::channel(false);
        Ok(ApiTask { stop, task: serve_task(state, rx) })
    }
}

fn serve_task(state: ApiState, mut rx: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let sd = async move {
            let _ = rx.wait_for(|s| *s).await;
        };
        if let Err(e) = api::serve(state, sd).await {
            tracing::error!("API server stopped: {e}");
        }
    })
}

/// A read API server started by [`Indexer::spawn_api`].
pub struct ApiTask {
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl ApiTask {
    /// Stop the server (open connections are asked to finish and close) and wait for it.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
    }
}

/// The follower and API tasks of a running indexer.
pub struct Running {
    stop: watch::Sender<bool>,
    follow: tokio::task::JoinHandle<()>,
    api: Option<tokio::task::JoinHandle<()>>,
}

impl Running {
    /// Ask both tasks to stop and wait for them.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.follow.await;
        if let Some(t) = self.api {
            let _ = t.await;
        }
    }
}

/// What [`replay_from_log`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayReport {
    /// Frames of the log (the database position after the replay).
    pub frames: u64,
    /// Relevant transactions applied.
    pub relevant: u64,
    /// Orders in the new database.
    pub orders: u64,
    /// Frames this build could not decode (kept in the log, skipped), with the reason.
    pub skipped: Vec<(u64, String)>,
    /// Reveals / holdings / imports of layouts this build does not have, dropped from decoded frames.
    pub dropped: record::DecodeStats,
}

/// Rebuild an EMPTY database from the record log (`kob-executor index replay`). The log is not
/// modified; the follower then resumes from the logged cursor and re-syncs the tail from the node. A frame this build
/// cannot decode (a newer record-log format, a template layout it has no table for) is skipped with a warning and
/// counted in the report, not fatal: the log is the permanent store and outlives the templates it recorded.
pub fn replay_from_log(cfg: &IndexerConfig) -> Result<ReplayReport, IndexerError> {
    let _writer = lock::WriterLock::acquire(cfg)?;
    if cfg.db_path().exists() {
        return Err(IndexerError::Config(format!(
            "{} already exists; replay only builds a new database (move the old one aside first)",
            cfg.db_path().display()
        )));
    }
    let log = recordlog::read_all(&cfg.records_dir())?;
    if log.frames == 0 {
        return Err(IndexerError::Config("record log is empty".into()));
    }
    if log.torn {
        tracing::warn!("the last record-log frame is torn (crash during append); it is ignored");
    }
    for (n, reason) in &log.skipped {
        tracing::warn!(frame = n, %reason, "record-log frame skipped: this build cannot decode it (it stays in the log)");
    }
    if log.dropped.dropped() > 0 {
        tracing::warn!(dropped = ?log.dropped, "record-log items of template layouts this build does not have were skipped");
    }
    let Some((_, first)) = log.records.first() else {
        return Err(IndexerError::Config(format!("none of the {} record-log frames could be decoded by this build", log.frames)));
    };
    let start = first.start;
    let tokens = load_tokens(cfg)?;
    let conn = db::open_writer(&cfg.db_path(), &cfg.network)?;
    let mut ing = Ingest::new(conn, processor(cfg, tokens), None).with_config(ingest_config(cfg));
    ing.init_cursor(&ingest::Cursor { hash: start, daa: 0 })?;
    let (frames, relevant, orders) = ing.replay_frames(log.records, log.frames)?;
    Ok(ReplayReport { frames, relevant, orders, skipped: log.skipped, dropped: log.dropped })
}

/// What [`replay_onto_database`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardReport {
    /// The database position before (`records_next_n`): the first frame applied.
    pub from: u64,
    /// The replay of the frames from `from` on (`frames` is the log's end, the database position after).
    pub replay: ReplayReport,
}

/// Bring an EXISTING database that is older than the record log (restored from a backup) forward: apply the frames
/// from its `records_next_n` to the end of the log (`kob-executor index replay --onto-database`). Neither the log nor the
/// frames the database already holds are touched; the follower then resumes from the last logged cursor. A database at
/// position 0 is refused (rebuild it with a plain `replay`), as is one ahead of the log.
pub fn replay_onto_database(cfg: &IndexerConfig) -> Result<ForwardReport, IndexerError> {
    let _writer = lock::WriterLock::acquire(cfg)?;
    if !cfg.db_path().exists() {
        return Err(IndexerError::Config(format!(
            "{} does not exist; `replay --onto-database` brings an existing database forward (use `replay` to build one)",
            cfg.db_path().display()
        )));
    }
    let conn = db::open_writer(&cfg.db_path(), &cfg.network)?;
    let from = ingest::records_next_n(&conn)?;
    if from == 0 {
        return Err(IndexerError::Config(
            "the database holds no record-log frame: move it aside and rebuild it with `kob-executor index replay`".into(),
        ));
    }
    let log = recordlog::read_from(&cfg.records_dir(), from)?;
    if log.frames < from {
        return Err(recordlog::RecordLogError::BehindDatabase { log_next: log.frames, db_next: from }.into());
    }
    if log.frames == from {
        let orders = Ingest::new(conn, processor(cfg, load_tokens(cfg)?), None).order_count()?;
        let replay = ReplayReport { frames: from, relevant: 0, orders, skipped: vec![], dropped: Default::default() };
        return Ok(ForwardReport { from, replay });
    }
    if log.torn {
        tracing::warn!("the last record-log frame is torn (crash during append); it is ignored");
    }
    for (n, reason) in &log.skipped {
        tracing::warn!(frame = n, %reason, "record-log frame skipped: this build cannot decode it (it stays in the log)");
    }
    let tokens = load_tokens(cfg)?;
    let mut ing = Ingest::new(conn, processor(cfg, tokens), None).with_config(ingest_config(cfg));
    check_lineage(&ing, &log, from)?;
    // each frame commits together with the position after it: an interrupted run continues where it stopped
    let (frames, relevant, orders) = ing.replay_frames(log.records, log.frames)?;
    // `dropped` counts the decoded frames from `from` on
    Ok(ForwardReport { from, replay: ReplayReport { frames, relevant, orders, skipped: log.skipped, dropped: log.dropped } })
}

/// The frame at the database position must continue the database: its `start` is the cursor the database ends at.
fn check_lineage(ing: &Ingest, log: &recordlog::Replay, from: u64) -> Result<(), IndexerError> {
    let cursor = ing.cursor()?;
    let Some((n, first)) = log.records.first().filter(|(n, _)| *n == from) else {
        return Err(IndexerError::Config(format!(
            "record-log frame {from} cannot be decoded by this build, so it cannot be checked against the database's cursor; \
             nothing was changed (rebuild the database from the log with `kob-executor index replay` into a fresh data directory)"
        )));
    };
    if cursor.as_ref().map(|c| c.hash) != Some(first.start) {
        return Err(recordlog::RecordLogError::Lineage {
            n: *n,
            frame_start: first.start,
            db_cursor: cursor.map_or_else(|| "unset".to_string(), |c| c.hash.to_string()),
        }
        .into());
    }
    Ok(())
}

/// Applies the record-log frames from `from` on to the database (one transaction per frame, the position with it) after the
/// lineage check, through a connection of its own and without a log attached (nothing is appended).
fn apply_frames_from(cfg: &IndexerConfig, tokens: Arc<TokenAllowlist>, from: u64) -> Result<(), IndexerError> {
    let log = recordlog::read_from(&cfg.records_dir(), from)?;
    let conn = db::open_writer(&cfg.db_path(), &cfg.network)?;
    let mut ing = Ingest::new(conn, processor(cfg, tokens), None).with_config(ingest_config(cfg));
    check_lineage(&ing, &log, from)?;
    ing.replay_frames(log.records, log.frames)?;
    Ok(())
}
