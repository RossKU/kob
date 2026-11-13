//! The KOB indexer: follows the selected chain of the node, tracks KOB orders and the token
//! holdings they trade, and keeps a reorg-safe database that the read API serves.
//!
//! The data flows through these stages:
//! 1. `rpc` reads the node (VSPC v2, High verbosity).
//! 2. [`follower`] turns the accepted chain into batches.
//! 3. [`ingest`] commits each batch in one atomic SQLite commit. The extracted KOB records are
//!    appended to the permanent record log first.
//! 4. [`processor`] runs the order and token state machine over [`record::TxRecord`]s.
//! 5. `reads` answers queries, and `api` serves them as REST and WebSocket.
//!
//! Raw node responses are never stored.

pub mod activity;
pub mod archive;
pub mod book;
pub mod candles;
pub mod db;
pub mod flags;
pub mod follower;
pub mod ids;
pub mod income;
pub mod ingest;
pub mod kcc23;
pub mod lean;
pub mod lock;
pub mod market;
pub mod outputs;
pub mod packed;
pub mod packed_dicts;
pub mod pairs;
pub mod prefetch;
pub mod processor;
pub mod reads;
pub mod record;
pub mod recordlog;
pub mod recovery;
pub mod release;
pub mod status;
pub mod storage;
pub mod trust;

use crate::api::{self, ApiState};
use crate::config::IndexerConfig;
use crate::rpc::ChainSource;
use crate::tokens::TokenAllowlist;
use follower::{Follower, FollowerConfig};
use ingest::{Ingest, IngestConfig};
use processor::Processor;
use recordlog::{LogBatch, RecordLog};
use status::{HealthState, IndexEvent};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, watch};

/// The data generation of the store.
///
/// It is bumped after every change that a read can see: a committed batch, a revert or an operator
/// import (`Ingest`), and a possibly-frozen flag (`executor::IndexerSource`). Two reads at one
/// generation read the same rows, which is what the response cache of the API keys on
/// (`api::cache`).
#[derive(Clone, Debug, Default)]
pub struct DataGeneration(Arc<std::sync::atomic::AtomicU64>);

impl DataGeneration {
    /// The current generation.
    pub fn get(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Bumps the generation after a change is committed, never before. A read that saw the old
    /// generation must not see older rows than it.
    pub fn bump(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// What can go wrong when the indexer starts or replays the record log.
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

/// The token allowlist of `--tokens`, or the default when there is none.
///
/// Without `--tokens`, a deployment build that pins a registry (`deploy-mainnet`) uses the
/// registry it embeds, which is the pinned one. Any other build lists nothing. A loaded file whose
/// hash differs from the pin is allowed, because operators may run their own list, and is logged.
/// `GET /v1/tokens` reports the hash either way.
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
        archive: (!cfg.lean && cfg.archive_pruning).then(|| archive::ArchiveConfig::new(cfg.spent_holdings_retention_daa())),
    }
}

fn processor(cfg: &IndexerConfig, tokens: Arc<TokenAllowlist>) -> Processor {
    Processor { tokens, rules: cfg.rules.clone() }
}

impl Indexer {
    /// Open the database and record log, verify they agree, and build the write path.
    pub fn open(mut cfg: IndexerConfig) -> Result<Indexer, IndexerError> {
        if cfg.lean {
            // Lean storage keeps no record log.
            cfg.records.enabled = false;
        }
        let writer = lock::WriterLock::acquire(&cfg)?;
        let tokens = load_tokens(&cfg)?;
        let conn = db::open_writer_with(&cfg.db_path(), &cfg.network, cfg.lean)?;
        if !cfg.lean && cfg.archive_pruning {
            // The index that the archive pruning reads. It takes a few seconds once, on a store of
            // millions of holdings.
            archive::ensure_indexes(&conn)?;
        }
        let mut db_next = ingest::records_next_n(&conn)?;
        let log = if cfg.records.enabled {
            let dir = cfg.records_dir();
            if db_next == 0 && recordlog::record_count(&dir)? > 0 {
                return Err(IndexerError::Config(format!(
                    "record log {} has frames but the database has none: run `kobd db replay` to rebuild the database from it, or move the log away to start fresh",
                    dir.display()
                )));
            }
            let (log, report) = match RecordLog::open(&dir, cfg.records.segment_bytes, db_next) {
                // One complete frame past the database is either the append that an unclean
                // shutdown left uncommitted, or a committed frame that a database restored from a
                // backup one frame older lacks. Either way it records the commit this database
                // misses. It is applied when it continues the cursor of the database and refused
                // otherwise, and it is never cut.
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
        // Build the candle aggregates of stored fills that have none. It takes one transaction per
        // token and leaves a store that has the aggregates alone.
        let started = std::time::Instant::now();
        let backfilled = ingest.backfill_candles()?;
        if backfilled.tokens > 0 {
            tracing::info!(
                tokens = backfilled.tokens,
                rows = backfilled.rows,
                seconds = started.elapsed().as_secs_f64(),
                "built the candle aggregates of the stored fills"
            );
        }
        let health = Arc::new(HealthState::new(&cfg.network));
        health.update(|h| {
            h.lag_tolerance_daa = cfg.lag_tolerance_daa();
            h.lean = cfg.lean;
        });
        // A store that began after the oldest history keeps saying so across restarts.
        if let Some(from) = ingest.history_from_daa()? {
            health.update(|h| {
                h.book_complete = false;
                h.history_from_daa = Some(from);
            });
        }
        // The update notice survives a restart, until this build is that release.
        if let Some(n) = ingest.newer_release_seen()? {
            release::publish(&health, n);
        }
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

    /// A follower of `source` that writes to this indexer's store.
    pub fn follower<S: ChainSource>(&self, source: Arc<S>) -> Follower<S> {
        Follower::new(source, self.ingest.clone(), FollowerConfig::from_indexer(&self.cfg), self.health.clone(), self.events.clone())
    }

    /// Builds the state of the read API over this indexer's store.
    ///
    /// It fails when the API configuration is invalid.
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
            min_order_value_sompi: self.cfg.rules.min_order_value_sompi,
            logo: if self.cfg.logo_reports.enabled {
                Some(crate::logo_reports::LogoReports::open(
                    &self.cfg.db_path(),
                    &self.cfg.network,
                    self.cfg.logo_reports.clone(),
                    self.cfg.api.rate_limit.ipv6_prefix_bits,
                )?)
            } else {
                None
            },
            price: self.cfg.api.usd_price.then(crate::api::price::UsdPrice::live),
        };
        st.validate().map_err(IndexerError::Config)?;
        if let Some(w) = self.cfg.api.proxy_warning() {
            tracing::warn!("{w}");
        }
        Ok(st)
    }

    /// Follows the node and serves the API until `shutdown` resolves, then flushes the database.
    pub async fn run(self, shutdown: impl std::future::Future<Output = ()> + Send + 'static) -> Result<(), IndexerError> {
        // With `[failover]`, the public pair takes over when the configured node stops answering,
        // see `rpc::failover`.
        let running = if self.cfg.failover.enabled() {
            self.spawn(crate::index_cli::failover_client(&self.cfg))?
        } else {
            self.spawn(crate::index_cli::node_client(&self.cfg))?
        };
        shutdown.await;
        tracing::info!("shutting down");
        running.stop().await;
        self.flush().await;
        Ok(())
    }

    /// Writes the write-ahead log into the database file and cuts it back, so that the file alone
    /// is the whole store.
    ///
    /// Call it at a clean stop, after the follower and the API have stopped. After a kill, the next
    /// open replays the log, which is also safe. The call waits at most 3 s, so a reader that
    /// holds the log open cannot hold the stop.
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

    /// Starts the follower and, when enabled, the API server on the current runtime.
    ///
    /// The indexer stays usable (`ingest`, `health`) while they run: `kobd --executor` reads the
    /// book through the same `ingest` handle. The follower moves the store past a gap that the
    /// pruning of the node caused instead of halting at it, see [`recovery`].
    pub fn spawn<S: ChainSource>(&self, source: Arc<S>) -> Result<Running, IndexerError> {
        // Validate the API configuration and take its address before anything is started. A busy
        // port then fails the start instead of leaving a process without its API.
        let api = if self.cfg.api.enabled { Some((self.api_state()?, bind_api(&self.cfg.api)?)) } else { None };
        let mut running = self.spawn_recovering(source);
        if let Some((state, listener)) = api {
            running.api = Some(serve_task(state, listener, running.stop.subscribe()));
        }
        Ok(running)
    }

    /// Starts the follower alone, without an API server.
    ///
    /// The fast start runs the API itself ([`crate::faststart`]), over whichever of its two
    /// stores is current.
    pub fn spawn_follower<S: ChainSource>(&self, source: Arc<S>) -> Running {
        let follower = Arc::new(self.follower(source));
        let (stop_tx, stop_rx) = watch::channel(false);
        let follow = tokio::spawn(async move { follower.run(stop_rx).await });
        Running { stop: stop_tx, follow, api: None }
    }

    /// Like [`Indexer::spawn_follower`], except that a gap the pruning of the node caused does not
    /// halt the follower.
    ///
    /// The store is moved to the pruning point of the node, or to the sink for a store that starts
    /// there. A new follower continues from there, a bounded number of times. The log and the
    /// health report the skipped chain, see [`recovery`].
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

    /// Starts the read API server over this indexer's store, whatever `api.enabled` says.
    pub fn spawn_api(&self) -> Result<ApiTask, IndexerError> {
        let state = self.api_state()?;
        let listener = bind_api(&self.cfg.api)?;
        let (stop, rx) = watch::channel(false);
        Ok(ApiTask { stop, task: serve_task(state, listener, rx) })
    }
}

/// Binds the address of the read API now, on the current runtime.
///
/// A port that another process holds is an error of the start, not a log line of a task that then
/// ends.
fn bind_api(cfg: &crate::config::ApiConfig) -> Result<tokio::net::TcpListener, IndexerError> {
    let refuse = |e: std::io::Error| {
        IndexerError::Config(format!(
            "the read API cannot listen on {}: {e} (another process holds the port? set [api] listen to a free address, or disable the API)",
            cfg.listen
        ))
    };
    let std_listener = std::net::TcpListener::bind(cfg.listen).map_err(refuse)?;
    std_listener.set_nonblocking(true).map_err(refuse)?;
    tokio::net::TcpListener::from_std(std_listener).map_err(refuse)
}

fn serve_task(state: ApiState, listener: tokio::net::TcpListener, mut rx: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
    let health = state.health.clone();
    tokio::spawn(async move {
        let stopping = rx.clone();
        let sd = async move {
            let _ = rx.wait_for(|s| *s).await;
        };
        match listener.local_addr() {
            Ok(a) => tracing::info!("read API listening on {a}"),
            Err(e) => tracing::warn!("read API: {e}"),
        }
        let r = api::serve_listener(listener, state, sd).await;
        if *stopping.borrow() {
            if let Err(e) = r {
                tracing::warn!("API server stopped: {e}");
            }
            return;
        }
        // The API ended while the process runs. Say why where a monitor and `kobd --executor` look.
        // The executor exits on it.
        let why = match r {
            Err(e) => e.to_string(),
            Ok(()) => "the server ended".to_string(),
        };
        tracing::error!("API server stopped: {why}");
        health.update(|h| h.api_error = Some(why));
    })
}

/// A read API server started by [`Indexer::spawn_api`].
pub struct ApiTask {
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl ApiTask {
    /// Stops the server and waits for it. Open connections are asked to finish and close.
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
    /// Asks both tasks to stop and waits for them.
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
    /// Reveals, holdings and imports of layouts this build does not have, dropped from decoded
    /// frames.
    pub dropped: record::DecodeStats,
}

/// Rebuilds an EMPTY database from the record log (`kobd db replay`).
///
/// The log is not modified. The follower then resumes from the logged cursor and re-syncs the tail
/// from the node. A frame that this build cannot decode is skipped with a warning and counted in
/// the report. This is not fatal, because the log is the permanent store and outlives the
/// templates it recorded. Such a frame is in a newer record-log format or has a template layout
/// that this build has no table for.
pub fn replay_from_log(cfg: &IndexerConfig) -> Result<ReplayReport, IndexerError> {
    let _writer = lock::WriterLock::acquire(cfg)?;
    if cfg.db_path().exists() {
        return Err(IndexerError::Config(format!(
            "{} already exists; replay only builds a new database (move the old one aside first)",
            cfg.db_path().display()
        )));
    }
    let dir = cfg.records_dir();
    // Verify the chain of the whole log first, decoding and keeping nothing. A broken log is
    // refused before a database exists.
    let end = recordlog::verify(&dir)?;
    if end.frames == 0 {
        return Err(IndexerError::Config("record log is empty".into()));
    }
    if end.torn {
        tracing::warn!("the last record-log frame is torn (crash during append); it is ignored");
    }
    // The frames are applied as they are decoded, so memory holds one frame whatever the size of
    // the log. The database is created at the first frame this build decodes, whose `start` is its
    // first cursor.
    let mut ing: Option<Ingest> = None;
    let mut run = ingest::ReplayRun::default();
    let streamed = recordlog::for_each_from(&dir, 0, Some(end.frames), |n, rec| {
        let ing = match &mut ing {
            Some(ing) => ing,
            None => {
                let tokens = load_tokens(cfg)?;
                let conn = db::open_writer(&cfg.db_path(), &cfg.network)?;
                let new = Ingest::new(conn, processor(cfg, tokens), None).with_config(ingest_config(cfg));
                new.init_cursor(&ingest::Cursor { hash: rec.start, daa: 0 })?;
                ing.insert(new)
            }
        };
        ing.replay_frame(&mut run, n, rec)?;
        Ok::<_, IndexerError>(())
    })?;
    for (n, reason) in &streamed.skipped {
        tracing::warn!(frame = n, %reason, "record-log frame skipped: this build cannot decode it (it stays in the log)");
    }
    if streamed.dropped.dropped() > 0 {
        tracing::warn!(dropped = ?streamed.dropped, "record-log items of template layouts this build does not have were skipped");
    }
    let Some(mut ing) = ing else {
        return Err(IndexerError::Config(format!("none of the {} record-log frames could be decoded by this build", end.frames)));
    };
    let (frames, relevant, orders) = ing.replay_finish(run, end.frames)?;
    // Prune the archive of what is final at the last cursor of the log, see `archive`. The follower
    // prunes the rest as it applies blocks. A log keeps only the chain blocks of its transactions,
    // so a rebuild may end with none in the window, and nothing is final until it does.
    ing.archive_prune_all()?;
    Ok(ReplayReport { frames, relevant, orders, skipped: streamed.skipped, dropped: streamed.dropped })
}

/// What [`replay_onto_database`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardReport {
    /// The database position before (`records_next_n`): the first frame applied.
    pub from: u64,
    /// The replay of the frames from `from` on.
    ///
    /// Its `frames` is the end of the log, which is the database position after the replay.
    pub replay: ReplayReport,
}

/// Brings an EXISTING database that is older than the record log forward, for example one
/// restored from a backup (`kobd db replay --onto-database`).
///
/// It applies the frames from the `records_next_n` of the database to the end of the log. Neither
/// the log nor the frames the database already holds are touched. The follower then resumes from
/// the last logged cursor. A database at position 0 is refused, as is one ahead of the log. Rebuild
/// a database at position 0 with a plain `replay`.
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
            "the database holds no record-log frame: move it aside and rebuild it with `kobd db replay`".into(),
        ));
    }
    let dir = cfg.records_dir();
    // Verify the chain of the whole log first. A broken log is refused before a frame is applied.
    let end = recordlog::verify(&dir)?;
    if end.frames < from {
        return Err(recordlog::RecordLogError::BehindDatabase { log_next: end.frames, db_next: from }.into());
    }
    if end.frames == from {
        let orders = Ingest::new(conn, processor(cfg, load_tokens(cfg)?), None).order_count()?;
        let replay = ReplayReport { frames: from, relevant: 0, orders, skipped: vec![], dropped: Default::default() };
        return Ok(ForwardReport { from, replay });
    }
    if end.torn {
        tracing::warn!("the last record-log frame is torn (crash during append); it is ignored");
    }
    let tokens = load_tokens(cfg)?;
    let mut ing = Ingest::new(conn, processor(cfg, tokens), None).with_config(ingest_config(cfg));
    // Each frame commits together with the position after it, so an interrupted run continues
    // where it stopped.
    let (streamed, (frames, relevant, orders)) = apply_frames(&mut ing, &dir, from, Some(end.frames), true)?;
    ing.archive_prune_all()?;
    // `dropped` counts the decoded frames from `from` on.
    Ok(ForwardReport { from, replay: ReplayReport { frames, relevant, orders, skipped: streamed.skipped, dropped: streamed.dropped } })
}

/// The cursor after the last record-log frame the database applied (`log_cursor_hash`,
/// `log_cursor_daa`), or `None` for a store whose build did not record its hash.
fn logged_cursor(conn: &rusqlite::Connection) -> Result<Option<ingest::Cursor>, IndexerError> {
    let hash = db::meta_get(conn, "log_cursor_hash").map_err(db::DbError::from)?.and_then(|h| crate::hex::Hash32::parse(&h).ok());
    let daa = db::meta_get(conn, "log_cursor_daa").map_err(db::DbError::from)?.and_then(|d| d.parse().ok());
    Ok(hash.zip(daa).map(|(hash, daa)| ingest::Cursor { hash, daa }))
}

/// The cursor after record-log frame `n`, or `None` when this build cannot decode that frame.
fn frame_cursor(dir: &std::path::Path, n: u64) -> Result<Option<ingest::Cursor>, IndexerError> {
    let mut out = None;
    recordlog::for_each_from(dir, n, Some(n + 1), |_, b| {
        out = Some(ingest::Cursor { hash: b.cursor.hash, daa: b.cursor.daa });
        Ok::<_, IndexerError>(())
    })?;
    Ok(out)
}

/// What the database knows of its place in the record log: its cursor, and the cursor after the
/// last frame it applied ([`logged_cursor`]).
struct DbPosition {
    cursor: Option<ingest::Cursor>,
    logged: Option<ingest::Cursor>,
}

/// Checks that the frame at the database position continues the database.
///
/// It does when either holds:
/// - The `start` of that frame is the cursor that the database ends at.
/// - The database moved past the last frame it applied, and the frame before this one
///   (`from - 1`) ends at the cursor that the database recorded with that frame. A batch without
///   KOB rows is not logged but still moves the cursor, so a database saved between two frames is
///   past the last frame it holds and short of the `start` of the next one. Only batches the log
///   leaves out lie between the two, and a replay from the log never applies them either. A
///   database still at the end of its last frame must be continued exactly (the first check), and
///   a store whose build did not record that cursor has only the first check.
///
/// `first` is the first frame from `from` on that this build decoded, and is `None` when none
/// could be decoded.
fn check_lineage(dir: &std::path::Path, db: &DbPosition, from: u64, first: Option<(u64, &LogBatch)>) -> Result<(), IndexerError> {
    let cursor = db.cursor.as_ref();
    let Some((n, first)) = first.filter(|(n, _)| *n == from) else {
        return Err(IndexerError::Config(format!(
            "record-log frame {from} cannot be decoded by this build, so it cannot be checked against the database's cursor; \
             nothing was changed (rebuild the database from the log with `kobd db replay` into a fresh data directory)"
        )));
    };
    // A database still at the end of its last frame must be continued exactly.
    let moved_on = cursor.is_some() && db.logged.is_some() && cursor != db.logged.as_ref();
    let continues = cursor.map(|c| c.hash) == Some(first.start) || (from > 0 && moved_on && frame_cursor(dir, from - 1)? == db.logged);
    if !continues {
        return Err(recordlog::RecordLogError::Lineage {
            n,
            frame_start: first.start,
            db_cursor: cursor.map_or_else(|| "unset".to_string(), |c| c.hash.to_string()),
        }
        .into());
    }
    Ok(())
}

/// Applies the record-log frames from `from` on, before `upto`, to the database as they are
/// decoded, so one is in memory at a time.
///
/// Each frame is one transaction together with the position after it. Nothing is applied when the
/// first frame fails the lineage check. `warn_skipped` warns of each frame that this build could
/// not decode. It returns what the read skipped and dropped, and the frames, relevant transactions
/// and orders.
fn apply_frames(
    ing: &mut Ingest,
    dir: &std::path::Path,
    from: u64,
    upto: Option<u64>,
    warn_skipped: bool,
) -> Result<(recordlog::Streamed, (u64, u64, u64)), IndexerError> {
    let db = DbPosition { cursor: ing.cursor()?, logged: logged_cursor(ing.conn())? };
    let mut run = ingest::ReplayRun::default();
    let streamed = recordlog::for_each_from(dir, from, upto, |n, rec| {
        if run.is_empty() {
            check_lineage(dir, &db, from, Some((n, &rec)))?;
        }
        ing.replay_frame(&mut run, n, rec)?;
        Ok::<_, IndexerError>(())
    })?;
    if warn_skipped {
        for (n, reason) in &streamed.skipped {
            tracing::warn!(frame = n, %reason, "record-log frame skipped: this build cannot decode it (it stays in the log)");
        }
    }
    if run.is_empty() {
        check_lineage(dir, &db, from, None)?;
    }
    let done = ing.replay_finish(run, streamed.frames)?;
    Ok((streamed, done))
}

/// Applies the record-log frames from `from` on to the database, after the lineage check.
///
/// Each frame is one transaction together with the position after it. It uses a connection of its
/// own and no log, so nothing is appended.
fn apply_frames_from(cfg: &IndexerConfig, tokens: Arc<TokenAllowlist>, from: u64) -> Result<(), IndexerError> {
    let conn = db::open_writer(&cfg.db_path(), &cfg.network)?;
    let mut ing = Ingest::new(conn, processor(cfg, tokens), None).with_config(ingest_config(cfg));
    apply_frames(&mut ing, &cfg.records_dir(), from, None, false)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(dir: &std::path::Path, listen: std::net::SocketAddr) -> IndexerConfig {
        let tokens = dir.join("tokens.json");
        std::fs::write(&tokens, "[]").unwrap();
        let mut cfg = IndexerConfig {
            network: "testnet-10".into(),
            data_dir: dir.join("data"),
            tokens_path: Some(tokens),
            ..IndexerConfig::default()
        };
        cfg.api.listen = listen;
        cfg
    }

    /// The read API takes its port before anything starts. A port that another process holds fails
    /// the start, so a supervisor sees it, instead of leaving a process that trades with no API.
    /// An API that ends while the process runs says so in the health (`api_error`), and
    /// `kobd --executor` exits on it.
    #[tokio::test]
    async fn a_busy_api_port_fails_the_start_and_a_dead_api_is_in_the_health() {
        let dir = tempfile::tempdir().unwrap();
        let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let idx = Indexer::open(config(dir.path(), busy.local_addr().unwrap())).unwrap();
        let node = crate::testkit::MockNode::new("testnet-10");
        let e = idx.spawn(node.clone()).err().expect("refused");
        assert!(e.to_string().contains("cannot listen on"), "{e}");
        let e = idx.spawn_api().err().expect("refused");
        assert!(e.to_string().contains("cannot listen on"), "{e}");
        drop(busy);
        // A free port is served. The health has no API error, and `halted()` is quiet while it
        // serves.
        let api = idx.spawn_api().unwrap();
        assert!(idx.health.snapshot().api_error.is_none());
        api.stop().await;
        assert!(idx.health.snapshot().api_error.is_none(), "a requested stop is no failure");
        idx.health.update(|h| h.api_error = Some("accept failed".into()));
        let (now, why) = idx.health.snapshot().halted().expect("halted");
        assert!(now && why.contains("read API stopped"), "{why}");
    }
}
