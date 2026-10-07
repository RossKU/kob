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
pub mod lock;
pub mod market;
pub mod pairs;
pub mod prefetch;
pub mod processor;
pub mod reads;
pub mod record;
pub mod recordlog;
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
        verify_threads: ingest::verify_threads(cfg.verify_threads),
    }
}

fn processor(cfg: &IndexerConfig, tokens: Arc<TokenAllowlist>) -> Processor {
    Processor { tokens, rules: cfg.rules.clone() }
}

impl Indexer {
    /// Open the database and record log, verify they agree, and build the write path.
    pub fn open(cfg: IndexerConfig) -> Result<Indexer, IndexerError> {
        let writer = lock::WriterLock::acquire(&cfg)?;
        let tokens = load_tokens(&cfg)?;
        let conn = db::open_writer(&cfg.db_path(), &cfg.network)?;
        let db_next = ingest::records_next_n(&conn)?;
        let log = if cfg.records.enabled {
            let dir = cfg.records_dir();
            if db_next == 0 && recordlog::record_count(&dir)? > 0 {
                return Err(IndexerError::Config(format!(
                    "record log {} has frames but the database has none: run `kob-executor index replay` to rebuild the database from it, or move the log away to start fresh",
                    dir.display()
                )));
            }
            let (log, report) = RecordLog::open(&dir, cfg.records.segment_bytes, db_next)?;
            if report.dropped_uncommitted > 0 || report.torn_tail_removed {
                tracing::warn!(?report, "repaired the record log after an unclean shutdown");
            }
            Some(log)
        } else {
            None
        };
        let proc = processor(&cfg, tokens.clone());
        let ingest = Ingest::new(conn, proc, log).with_config(ingest_config(&cfg));
        let health = Arc::new(HealthState::new(&cfg.network));
        health.update(|h| h.lag_tolerance_daa = cfg.lag_tolerance_daa());
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
        let st = ApiState {
            pool,
            health: self.health.clone(),
            events: self.events.clone(),
            tokens: self.tokens.clone(),
            cfg: self.cfg.api.clone(),
            settle_depth_daa: self.cfg.settle_depth_daa,
            daa_per_second: self.cfg.daa_per_second,
            lag_alarm_hours: self.cfg.lag_alarm_hours.clone(),
        };
        st.validate().map_err(IndexerError::Config)?;
        Ok(st)
    }
    /// Follow the node and serve the API until `shutdown` resolves.
    pub async fn run(self, shutdown: impl std::future::Future<Output = ()> + Send + 'static) -> Result<(), IndexerError> {
        let running = self.spawn(crate::index_cli::node_client(&self.cfg))?;
        shutdown.await;
        tracing::info!("shutting down");
        running.stop().await;
        Ok(())
    }

    /// Start the follower and (when enabled) the API server on the current runtime. The indexer
    /// stays usable (`ingest`, `health`) while they run: `kob-executor run` reads the book through
    /// the same `ingest` handle.
    pub fn spawn<S: ChainSource>(&self, source: Arc<S>) -> Result<Running, IndexerError> {
        // validate the API configuration before anything is started
        let api_state = if self.cfg.api.enabled { Some(self.api_state()?) } else { None };
        let follower = Arc::new(self.follower(source));
        let (stop_tx, stop_rx) = watch::channel(false);
        let follow = tokio::spawn(async move { follower.run(stop_rx).await });
        let api_task = if let Some(state) = api_state {
            let mut rx = stop_tx.subscribe();
            Some(tokio::spawn(async move {
                let sd = async move {
                    let _ = rx.changed().await;
                };
                if let Err(e) = api::serve(state, sd).await {
                    tracing::error!("API server stopped: {e}");
                }
            }))
        } else {
            None
        };
        Ok(Running { stop: stop_tx, follow, api: api_task })
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
