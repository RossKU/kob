//! `kobd --executor`: the indexer, the matcher and the keepers in one process over one store.
//!
//! * the follower (VSPC v2, `High`) indexes the chain into SQLite and serves the read API, as
//!   `kobd --indexer` does;
//! * the runner (`matcher::run`) plans against the same store through [`IndexerSource`]: a single
//!   consistent read per tick ([`crate::indexer::book::snapshot`]), so the matcher never trades an
//!   order the API would not list;
//! * "done" means accepted: the transactions the runner submits are watched by the indexer's own
//!   follower ([`Ingest::watch_tx`]) and their acceptance, reorgs included, comes from the chain blocks
//!   the store was built from. There is no second VSPC follower in this process.
//!
//! * with `--x402-config`, the x402 facilitator serves in the same process: node RPC for UTXO facts
//!   and submission, and the same follower's acceptance tracking for finality
//!   ([`crate::x402::indexed::IndexedChain`]); it holds no key.
//!
//! The roles that hold a key (`--no-match` / `--no-keep` switch either off) run in the same process as
//! the public read API; deployments that want them apart run `index` and `match --book-file` /
//! `keep --book-file` instead (`docs/ops/executor.md`).

use crate::hex::Hash32;
use crate::index_cli::{build_config, node_client, shutdown_signal, IndexOpts};
use crate::indexer::book;
use crate::indexer::ingest::Ingest;
use crate::indexer::status::HealthState;
use crate::indexer::{Indexer, IndexerError};
use crate::keepers::KeeperConfig;
use crate::matcher::book::MemoryBook;
use crate::matcher::engine::EngineConfig;
use crate::matcher::node::{MultiSubmit, NodeApi, WrpcConfig, WrpcNode};
use crate::matcher::run::{BookSource, Roles, RunConfig, Runner};
use crate::matcher::wallet::{HotKey, Signer};
use crate::rpc::ChainSource;
use anyhow::{bail, Context, Result};
use clap::Args;
use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The runner's book source over the indexer's store and chain follower.
pub struct IndexerSource {
    ingest: Arc<Mutex<Ingest>>,
    /// The follower's health: the source is ready only while the store is caught up with the node or within its lag tolerance.
    health: Option<Arc<HealthState>>,
    /// A read-only connection of the source's own the book is read on ([`IndexerSource::with_reader`]); without one, the
    /// writer's connection under the ingest lock.
    reader: Option<Arc<Mutex<rusqlite::Connection>>>,
}

impl IndexerSource {
    /// A source over the store alone (always ready: the caller keeps the store current).
    pub fn new(ingest: Arc<Mutex<Ingest>>) -> Self {
        IndexerSource { ingest, health: None, reader: None }
    }

    /// Read the book on `conn`, a read-only connection to the store's file (`db::open_reader`), on the blocking thread pool
    /// ([`BookSource::snapshot_read`]), instead of on the writer's connection under the ingest lock. The read is one read
    /// transaction, so it sees one committed batch, as before: the follower commits each batch in one transaction, nothing
    /// the book reads is ever left uncommitted between batches, and the matcher's own state the lock guards (the watched
    /// transactions, `Ingest::watch_tx`) is not part of the book. What changes: the follower commits while the book is read
    /// (WAL), where it used to wait for the read, and the read no longer stalls a runtime worker.
    pub fn with_reader(mut self, conn: rusqlite::Connection) -> Self {
        self.reader = Some(Arc::new(Mutex::new(conn)));
        self
    }

    /// Ready only while the follower reports the store caught up ([`HealthSnapshot::caught_up`]) or within the lag tolerance
    /// the health state carries ([`HealthSnapshot::within_lag_tolerance`], `max_lag_secs`).
    ///
    /// [`HealthSnapshot::caught_up`]: crate::indexer::status::HealthSnapshot::caught_up
    pub fn with_health(mut self, health: Arc<HealthState>) -> Self {
        self.health = Some(health);
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Ingest> {
        // The follower commits under this lock for a few milliseconds per batch; a poisoned lock
        // (a panic in a commit) leaves SQLite consistent, so reading on is right.
        self.ingest.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl BookSource for IndexerSource {
    fn ready(&mut self) -> Result<(), String> {
        match self.health.as_ref().and_then(|h| h.snapshot().not_ready()) {
            Some(why) => Err(why),
            None => Ok(()),
        }
    }

    fn snapshot(&mut self, operator: &[u8; 32]) -> Result<MemoryBook> {
        match &self.reader {
            Some(r) => read_book(r, Some(*operator)),
            None => Ok(book::snapshot(self.lock().conn(), Some(*operator))?),
        }
    }

    fn snapshot_read(&mut self, operator: &[u8; 32]) -> Option<crate::matcher::run::BookRead> {
        let r = self.reader.clone()?;
        let op = *operator;
        Some(Box::new(move || read_book(&r, Some(op))))
    }

    fn watch(&mut self, txid: [u8; 32]) {
        self.lock().watch_tx(Hash32(txid));
    }

    fn operator_tokens(&mut self, operator: &[u8; 32]) -> Vec<crate::maintenance::OwnToken> {
        let g = self.lock();
        let listed = |t: &crate::maintenance::OwnToken| match t.token.utxo.covenant_id {
            Some(c) => g.processor().operator_token(g.conn(), &Hash32(c)),
            None => Ok(false),
        };
        book::operator_tokens(g.conn(), operator)
            .and_then(|all| {
                let mut out = Vec::with_capacity(all.len());
                for t in all {
                    if listed(&t)? {
                        out.push(t);
                    }
                }
                Ok(out)
            })
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "cannot read the operator's tokens from the store; no maintenance this tick");
                vec![]
            })
    }

    fn report_frozen(&mut self, daa: u64, flagged: &[([u8; 32], String)], cleared: &[[u8; 32]]) {
        let guard = self.lock();
        for (id, why) in flagged {
            if let Err(e) = crate::indexer::flags::set(guard.conn(), id, why, daa) {
                tracing::warn!(error = %e, "cannot store a possibly-frozen flag");
            }
        }
        for id in cleared {
            if let Err(e) = crate::indexer::flags::clear(guard.conn(), id) {
                tracing::warn!(error = %e, "cannot clear a possibly-frozen flag");
            }
        }
        if !flagged.is_empty() || !cleared.is_empty() {
            // the books, counts and order views read the flags: a new data generation for the API's response cache
            guard.generation().bump();
        }
    }

    fn acceptance(&mut self, tracked: &[[u8; 32]]) -> Option<BTreeMap<[u8; 32], (String, u64)>> {
        let ids: Vec<Hash32> = tracked.iter().map(|t| Hash32(*t)).collect();
        let seen = self.lock().watched_acceptance(&ids);
        Some(seen.into_iter().filter_map(|(t, a)| a.map(|(h, daa)| (t.0, (h.to_hex(), daa)))).collect())
    }

    fn metrics(&mut self) -> String {
        self.health.as_ref().map(|h| h.snapshot().metrics_text()).unwrap_or_default()
    }

    fn status(&mut self) -> Option<String> {
        self.health.as_ref().map(|h| h.snapshot().status_line())
    }

    fn fatal(&mut self) -> Option<(bool, String)> {
        self.health.as_ref().and_then(|h| h.snapshot().halted())
    }

    fn report_executor(&mut self, health: crate::indexer::status::ExecutorHealth, metrics: String) {
        if let Some(h) = &self.health {
            h.update(|s| {
                s.executor = Some(health);
                s.executor_metrics = metrics;
            });
        }
    }
}

/// One consistent read of the book ([`book::snapshot`]: one read transaction) on a reader connection.
fn read_book(reader: &Mutex<rusqlite::Connection>, operator: Option<[u8; 32]>) -> Result<MemoryBook> {
    let conn = reader.lock().unwrap_or_else(|e| e.into_inner());
    Ok(book::snapshot(&conn, operator)?)
}

/// `kobd --executor`.
#[derive(Args, Debug, Clone)]
pub struct RunArgs {
    #[command(flatten)]
    pub index: IndexOpts,
    /// File holding the operator's secret key (64 hex, mode 0600). Not the key itself.
    #[arg(long, env = "KOB_OPERATOR_KEY_FILE")]
    pub key_file: Option<PathBuf>,
    /// Build and validate everything, submit nothing (nothing goes to the network: the x402 facilitator is not served either).
    #[arg(long)]
    pub dry_run: bool,
    /// Do not run the matcher (keepers only).
    #[arg(long)]
    pub no_match: bool,
    /// Do not run the keepers (matcher only).
    #[arg(long)]
    pub no_keep: bool,
    /// Tick period in milliseconds.
    #[arg(long, default_value_t = 1_000)]
    pub tick_ms: u64,
    /// The lowest fee rate, sompi per gram (>= 100): what every transaction pays without a fee estimate (`--no-fee-estimate`,
    /// or the node answers none). With one, each transaction pays its urgency's bucket within `--fee-max-rate` and
    /// `--fee-max-tx-kas` (`docs/ops/executor.md`, fee policy).
    #[arg(long, default_value_t = 100)]
    pub fee_rate: u64,
    #[command(flatten)]
    pub fees: crate::fee::FeeArgs,
    /// Kill switch: nothing is built or submitted while this file exists (the matcher, the keepers and the x402
    /// facilitator). A file whose state cannot be read (permission denied) or whose directory does not exist counts as present.
    /// Keep it on persistent storage, outside the data directory (a rebuild deletes that).
    #[arg(long)]
    pub pause_file: Option<PathBuf>,
    /// Prometheus textfile-collector output (`.prom`).
    #[arg(long)]
    pub metrics_file: Option<PathBuf>,
    /// Warn (and set `kob_operator_low_funds`) below this many KAS of funding.
    #[arg(long, default_value_t = 100)]
    pub low_funds_kas: u64,
    /// Plan nothing while the indexer is more than this many DAA behind the node (0: never wait). Default: the indexer's
    /// lag tolerance (`max_lag_secs`, 30 s = 300 DAA), 300 when that is 0.
    #[arg(long)]
    pub max_book_lag_daa: Option<u64>,
    /// Matcher: smallest profit per batch after its fee, sompi.
    #[arg(long, default_value_t = 1)]
    pub min_profit: i64,
    /// Matcher: optional cap on the size of one transaction, bytes (0, the default: the physical limit).
    #[arg(long, default_value_t = 0)]
    pub max_tx_bytes: u64,
    /// Matcher: do not chain steps onto unaccepted parents.
    #[arg(long)]
    pub no_chain: bool,
    /// Matcher: do not arm or ratchet stops with updates in the batches (the stops a batch's plain fills trigger are
    /// still filled next to their evidence).
    #[arg(long)]
    pub no_arm: bool,
    /// Matcher: wall-clock budget of one tick in milliseconds (0: unlimited); what is left waits for the next tick.
    #[arg(long, default_value_t = 20_000)]
    pub tick_budget_ms: u64,
    /// Matcher: optional cap on the candidates per (book, class, side) the planner looks at (0, the default: all).
    #[arg(long, default_value_t = 0)]
    pub max_candidates_per_group: usize,
    /// Matcher: most funding inputs of one batch (the largest UTXOs first until the batch is paid).
    #[arg(long, default_value_t = 8)]
    pub max_funding_inputs: usize,
    /// Matcher: above this many funding UTXOs each batch also merges the smallest into its change (see
    /// `--consolidate-funding`).
    #[arg(long, default_value_t = 4)]
    pub funding_target_utxos: usize,
    /// Matcher: most extra funding UTXOs one batch merges into its change while the pool is fragmented (0: off).
    #[arg(long, default_value_t = 2)]
    pub consolidate_funding: usize,
    /// Do not run the maintenance jobs on the operator's own token UTXOs (merge, sell; with the matcher).
    #[arg(long)]
    pub no_maintenance: bool,
    /// Maintenance: merge the operator's token UTXOs only, never sell them into the book.
    #[arg(long)]
    pub no_dust_sell: bool,
    /// Maintenance: most transactions per tick.
    #[arg(long, default_value_t = 2)]
    pub maintenance_max_jobs: usize,
    /// Matcher and maintenance: the surplus-inventory policy (strict JSON, `docs/ops/executor.md`, "Surplus inventory"):
    /// the pair surpluses the operator may accumulate as inventory (never sold by maintenance). Default: none (off).
    #[arg(long)]
    pub inventory_policy: Option<PathBuf>,
    /// Keepers: do not refund / kill / close.
    #[arg(long)]
    pub no_refund: bool,
    /// Keepers: sweep strays of the operator's own orders in place (the order continues unchanged).
    #[arg(long)]
    pub sweep_own_strays: bool,
    /// Keepers: return proven foreign strays (other tokens owned by an order id) to the maker inside the refund, kill or
    /// close that ends the order (permissionless; dropped from a job that would not pay with them).
    #[arg(long)]
    pub return_foreign_strays: bool,
    /// Keepers: smallest profit (tip - fee) per job, sompi.
    #[arg(long, default_value_t = 0)]
    pub keeper_min_profit: i64,
    #[command(flatten)]
    pub liveness: crate::keepers::LivenessArgs,
    #[command(flatten)]
    pub select: crate::matcher::select::SelectArgs,
    /// Also serve the x402 facilitator (strict JSON configuration, `crates/kobd/x402.example.json`).
    /// Its `node` is the indexer's `--rpc-url`; its `network` must be the indexer's. Not served with `--dry-run`; with it the
    /// first start has no fast start (its book is bound to one store).
    #[arg(long, env = "KOB_X402_CONFIG")]
    pub x402_config: Option<PathBuf>,
}

impl RunArgs {
    /// The x402 facilitator's configuration, when it is served: never in a dry run (`--dry-run` sends nothing to the network,
    /// and the facilitator settles payers' transactions there).
    pub fn x402(&self) -> Option<&std::path::Path> {
        self.x402_config.as_deref().filter(|_| !self.dry_run)
    }

    /// The runner's settings.
    pub fn run_config(&self, network: &str) -> RunConfig {
        let mut engine = EngineConfig::default();
        engine.planner.fee_rate = self.fee_rate;
        engine.planner.min_profit = self.min_profit;
        engine.planner.max_tx_bytes = self.max_tx_bytes;
        engine.planner.arm = !self.no_arm;
        engine.planner.max_candidates_per_group = self.max_candidates_per_group;
        engine.tick_budget_ms = self.tick_budget_ms;
        engine.chain_unconfirmed = !self.no_chain;
        engine.funding = crate::matcher::engine::FundingConfig {
            max_inputs: self.max_funding_inputs.max(1),
            target_utxos: self.funding_target_utxos,
            consolidate: self.consolidate_funding,
        };
        let maintenance = crate::maintenance::MaintenanceConfig {
            enabled: !self.no_maintenance,
            sell: !self.no_dust_sell,
            max_jobs: self.maintenance_max_jobs,
            fee_rate: self.fee_rate,
            token_carrier: engine.token_carrier,
            safety_margin: engine.planner.safety_margin,
            ..crate::maintenance::MaintenanceConfig::default()
        };
        let mut keeper = KeeperConfig {
            fee_rate: self.fee_rate,
            refund: !self.no_refund,
            sweep_own_strays: self.sweep_own_strays,
            return_foreign_strays: self.return_foreign_strays,
            min_profit: self.keeper_min_profit,
            ..KeeperConfig::default()
        };
        // `run` refuses invalid liveness options before it gets here
        let _ = self.liveness.apply(&mut keeper);
        RunConfig {
            maintenance,
            network: network.to_string(),
            roles: Roles { matcher: !self.no_match, keeper: !self.no_keep },
            engine,
            keeper,
            dry_run: self.dry_run,
            tick: Duration::from_millis(self.tick_ms.max(100)),
            pause_file: self.pause_file.clone(),
            metrics_file: self.metrics_file.clone(),
            low_funds: self.low_funds_kas * 100_000_000,
            max_book_lag: match self.max_book_lag_daa {
                Some(0) => None,
                Some(n) => Some(n),
                None => Some(300),
            },
            // `run` refuses invalid fee options before it gets here
            fee_policy: self.fees.policy(self.fee_rate).unwrap_or_else(|_| crate::fee::FeePolicy::fixed(self.fee_rate)),
            // `run` refuses invalid selection options before it gets here
            select: self.select.config().unwrap_or_default(),
            status_every: self.index.status_every(),
            ..RunConfig::default()
        }
    }
}

/// Runs the indexer (follower and API) and the runner on `chain` / `node` until `shutdown` resolves.
/// The runner reads the indexer's store and follows acceptance through it.
pub async fn run_with<C, N, S>(
    indexer: Indexer,
    chain: Arc<C>,
    node: N,
    signer: Box<dyn Signer>,
    cfg: RunConfig,
    shutdown: S,
) -> Result<()>
where
    C: ChainSource,
    N: NodeApi,
    S: Future<Output = ()>,
{
    run_with_x402(indexer, chain, node, signer, cfg, None, shutdown).await
}

/// [`run_with`] plus, when given, the x402 facilitator `x402` (built over the indexer's acceptance
/// tracking, see [`x402_service`]) serving until the same `shutdown`.
pub async fn run_with_x402<C, N, S>(
    indexer: Indexer,
    chain: Arc<C>,
    node: N,
    signer: Box<dyn Signer>,
    cfg: RunConfig,
    x402: Option<crate::x402::Service>,
    shutdown: S,
) -> Result<()>
where
    C: ChainSource,
    N: NodeApi,
    S: Future<Output = ()>,
{
    // the swap-and-pay settlements in flight reserve their orders: the matcher's batches plan around them
    let reservations: Option<crate::matcher::run::Reservations> = x402.as_ref().map(|svc| {
        let fac = svc.fac.clone();
        std::sync::Arc::new(move || fac.ledger.reserved_order_outpoints()) as crate::matcher::run::Reservations
    });
    // the runner's fee rates price the facilitator's intent executions and expiries
    let fee_board = x402.as_ref().map(|svc| svc.fac.fees.clone());
    let facilitator = match x402 {
        Some(svc) => {
            let listener = svc.bind().await.map_err(anyhow::Error::msg)?;
            let (stop, mut stopped) = tokio::sync::watch::channel(false);
            let task = tokio::spawn(crate::x402::serve_service(svc, listener, async move {
                let _ = stopped.wait_for(|s| *s).await;
            }));
            Some((stop, task))
        }
        None => None,
    };
    let (roles, lean) = (cfg.roles, indexer.cfg.lean);
    indexer.health.update(|h| h.roles = roles.health_roles(lean));
    let running = indexer.spawn(chain)?;
    // the book is read on a connection of its own, off the ingest lock and off the runtime's workers
    let reader = crate::indexer::db::open_reader(&indexer.cfg.db_path(), &indexer.cfg.network)?;
    let source = IndexerSource::new(indexer.ingest.clone()).with_health(indexer.health.clone()).with_reader(reader);
    let mut runner = Runner::new(node, source, signer, cfg);
    runner.reservations = reservations;
    runner.fee_board = fee_board;
    let res = runner.run(shutdown).await;
    tracing::info!("shutting down");
    if let Some((stop, task)) = facilitator {
        let _ = stop.send(true);
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!("x402 facilitator: {e}"),
            Err(e) => tracing::error!("x402 facilitator task: {e}"),
        }
    }
    running.stop().await;
    indexer.flush().await;
    res
}

/// The x402 facilitator of `kobd --executor`: the configuration at `path` with the indexer's node
/// and network, over [`IndexedChain`](crate::x402::indexed::IndexedChain) (node RPC + the indexer's
/// acceptance tracking). `pause_file` (`--pause-file`) disables it like its own `killSwitchFile`: nothing is verified,
/// settled or broadcast while it exists. With the indexer's `health`, intents are executed only against a book that is caught
/// up or within the lag tolerance (the same gate as the matcher's), and `/supported` offers them only then.
pub fn x402_service(
    path: &std::path::Path,
    index: &crate::config::IndexerConfig,
    ingest: Arc<Mutex<Ingest>>,
    pause_file: Option<PathBuf>,
    health: Option<Arc<HealthState>>,
) -> Result<crate::x402::Service> {
    use crate::x402::config::X402Config;
    let mut xcfg = X402Config::load(path)?;
    // absent from the file: the indexer's network (which `build_config` resolved), never an implicit one
    if xcfg.network.trim().is_empty() {
        xcfg.network = format!("kaspa:{}", index.network);
    }
    xcfg.node = index.rpc_url.clone();
    xcfg.pause_file = pause_file;
    let built = xcfg.build()?;
    open_auth_beside_api(xcfg.auth, &index.api)?;
    if built.network.registry_name() != index.network {
        bail!("x402: the facilitator is configured for {}, the indexer follows {}", built.network, index.network);
    }
    // its submissions go to every node of the indexer that takes them, as the matcher's do (a resolver pair: both nodes)
    let peers: Vec<String> = index.secondaries().iter().filter(|n| n.submit).map(|n| n.url.clone()).collect();
    let node = crate::x402::node_chain(&xcfg, &built)
        .map_err(anyhow::Error::msg)?
        .with_submit_urls(&peers, std::time::Duration::from_millis(xcfg.node_timeout_ms));
    let ready: Option<crate::x402::facilitator::BookReadiness> = health.map(|h| {
        Arc::new(move || match h.snapshot().not_ready() {
            Some(why) => Err(why),
            None => Ok(()),
        }) as crate::x402::facilitator::BookReadiness
    });
    let book_ready = ready.clone();
    // the intents' book is read on a connection of its own as well (one read transaction; never the writer's, never under
    // the ingest lock)
    let book_reader = Mutex::new(crate::indexer::db::open_reader(&index.db_path(), &index.network)?);
    let book: crate::x402::BookFn = Arc::new(move || {
        // a lagging store may list orders that are spent by now: no intent is planned against it
        if let Some(r) = &book_ready {
            r()?;
        }
        let b = read_book(&book_reader, None).map_err(|e| e.to_string())?;
        Ok(crate::x402::quote::book_view(&b))
    });
    let chain = crate::x402::indexed::IndexedChain::new(node, ingest);
    crate::x402::prepare_with(built, Arc::new(chain), Some(book), ready).map_err(anyhow::Error::msg)
}

/// An open facilitator (`auth: "open"`) is for a host where nothing relays connections to it. A read API that listens on a
/// public address, or that is configured behind a reverse proxy (`trusted_proxies`), says that this host serves the outside:
/// the facilitator of the same process is then refused unless it uses merchant keys.
pub fn open_auth_beside_api(auth: crate::x402::config::AuthMode, api: &crate::config::ApiConfig) -> Result<()> {
    if auth == crate::x402::config::AuthMode::Open
        && api.enabled
        && (!api.listen.ip().is_loopback() || !api.trusted_proxies.is_empty())
    {
        bail!(
            "x402: auth \"open\" is refused next to a read API that listens on {} or has trusted_proxies: this host serves the \
             outside, so use auth \"required\" with merchant keys",
            api.listen
        );
    }
    Ok(())
}

/// The runner's node: transactions go to every node that takes submissions (the primary's verdict counts), every other call to
/// the primary.
fn multi_submit(
    cfg: &crate::config::IndexerConfig,
    chain: &crate::rpc::multi::MultiNode<crate::rpc::WrpcClient>,
) -> MultiSubmit<WrpcNode> {
    let others = chain
        .specs()
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, s)| s.submit)
        .map(|(i, s)| (i, WrpcNode::new(WrpcConfig::new(s.url.clone()))))
        .collect();
    MultiSubmit::new(WrpcNode::new(WrpcConfig::new(cfg.primary_url().to_string())), others, Some(chain.board()))
}

/// The node failover (`[failover]`) is the Indexer's only: refused here rather than ignored. The Executor signs and submits:
/// its matcher reads the operator's coins, the fee estimate and the sync state from the configured node and its submissions
/// and acceptance tracking are bound to the nodes it was started with, so a switch of the chain source under it would mix two
/// views mid-trade. An Executor that should survive the loss of its node runs on the public pair itself (`rpc_url =
/// "resolver"`, which fails over within the pair), or on its own node with public `nodes` that take its submissions.
pub fn refuse_failover(cfg: &crate::config::IndexerConfig) -> Result<()> {
    if cfg.failover.enabled() {
        bail!(
            "[failover] is an option of the Indexer (`kobd --indexer`), not of the Executor: remove it, or run the Executor on the public nodes (rpc_url = \"resolver\": a cross-checked pair that fails over within itself); docs/ops/executor.md, Part C, *Node failover*"
        );
    }
    Ok(())
}

/// `path` as an absolute path without `.` and `..` components (relative to the working directory: the configuration's), in the
/// case Windows ignores: what [`inside`] compares. Lexical only: nothing needs to exist.
fn lexical(path: &std::path::Path) -> PathBuf {
    use std::path::Component;
    let abs = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    if cfg!(windows) {
        PathBuf::from(out.to_string_lossy().to_lowercase())
    } else {
        out
    }
}

/// Whether `file` lies inside the directory `dir` (lexically, [`lexical`]).
fn inside(file: &std::path::Path, dir: &std::path::Path) -> bool {
    let (f, d) = (lexical(file), lexical(dir));
    f != d && f.starts_with(&d)
}

/// The key file and the pause file must not lie inside the data directory (audit M-10): the data directory is what an operator
/// deletes or moves aside to rebuild the store (quickstart 7, step 6), and either file would go with it. The key file there is
/// refused (its funds would go with it). A pause file there (the setup of a build before 2026-11-11 wrote `kob-data/pause`)
/// is warned about while the data directory exists, and refused when it does not: the directory was deleted, so a pause the
/// operator had set went with it, and the start would recreate the directory and trade.
fn refuse_files_in_data_dir(
    key_file: Option<&std::path::Path>,
    pause_file: Option<&std::path::Path>,
    data_dir: &std::path::Path,
) -> Result<()> {
    if let Some(k) = key_file.filter(|k| inside(k, data_dir)) {
        bail!(
            "the key file {} is inside the data directory {}: a rebuild deletes the data directory (quickstart 7), and the key would go with it, and with it the funds. Move the key file next to kobd.toml (key_file = \"kob-operator.key\") and start again",
            k.display(),
            data_dir.display()
        );
    }
    if let Some(p) = pause_file.filter(|p| inside(p, data_dir)) {
        if !data_dir.exists() {
            bail!(
                "the pause file {} is inside the data directory {}, which does not exist (deleted for a rebuild?): a pause set there went with it, and this start would trade. Set pause_file = \"kob-pause\" in kobd.toml (a file next to it), create that file first if the executor should stay paused, and start again",
                p.display(),
                data_dir.display()
            );
        }
        tracing::warn!(
            pause_file = %p.display(),
            data_dir = %data_dir.display(),
            "the pause file is inside the data directory: deleting the data directory for a rebuild deletes the pause too. Set pause_file = \"kob-pause\" in kobd.toml (a file next to it)"
        );
    }
    Ok(())
}

/// `kobd --executor`.
pub async fn run(a: RunArgs) -> Result<()> {
    if a.no_match && a.no_keep {
        bail!("both roles are off: the Indexer alone is `kobd --indexer`");
    }
    let fee_policy = a.fees.policy(a.fee_rate).map_err(anyhow::Error::msg)?;
    a.liveness.apply(&mut KeeperConfig::default()).map_err(anyhow::Error::msg)?;
    a.select.config().map_err(anyhow::Error::msg)?;
    let cfg = build_config(&a.index)?;
    refuse_failover(&cfg)?;
    refuse_files_in_data_dir(a.key_file.as_deref(), a.pause_file.as_deref(), &cfg.data_dir)?;
    // the operator's funds are read from the nodes' UTXO index: a node without it is unusable (`rpc::judge`)
    crate::rpc::judge::require_utxo_index();
    // listening before the slow part (opening the store, the first calls to the node): a Ctrl+C there is not lost
    let shutdown = shutdown_signal();
    let signer = HotKey::load(a.key_file.as_deref())?;
    let mut run_cfg = a.run_config(&cfg.network);
    if let Some(path) = &a.inventory_policy {
        let p = crate::matcher::planner::InventoryPolicy::from_file(path).map_err(anyhow::Error::msg)?;
        tracing::info!(
            accept = p.accept_surplus_tokens,
            tokens = p.tokens.len(),
            haircut_bps = p.haircut_bps,
            keep_carrier = p.keep_carrier,
            "surplus inventory policy"
        );
        run_cfg.engine.planner.inventory = p.clone();
        run_cfg.maintenance.inventory = p;
        // the period's use survives a restart (a small file next to the database)
        run_cfg.inventory_state = Some(cfg.data_dir.join("inventory-use.json"));
    }
    if a.max_book_lag_daa.is_none() && cfg.lag_tolerance_daa() > 0 {
        // one bound for both gates: a store within the tolerance is a book the runner may plan against
        run_cfg.max_book_lag = Some(cfg.lag_tolerance_daa());
    }
    run_cfg.verify_live_min_lag = cfg.verify_live_min_lag_daa();
    // the in-flight journal, the keepers' subsidy window and the start count live next to the database (audit 2026-11-11
    // M-3, L-2)
    run_cfg.state_dir = Some(cfg.data_dir.clone());
    // the hot key's balance is not shown to other hosts unless the operator asks for it (`api.executor_funds`)
    run_cfg.report_funds = cfg.api.shows_executor_funds();
    if !run_cfg.report_funds && cfg.api.enabled {
        tracing::info!(listen = %cfg.api.listen, "the read API is reachable from other hosts: /v1/health and /v1/metrics leave the executor's balance out (api.executor_funds = true shows it)");
    }
    // a journal that cannot be read would let this run plan over the inputs of the last run's mempool transactions
    if !run_cfg.dry_run {
        let path = cfg.data_dir.join(crate::matcher::journal::FILE_NAME);
        crate::matcher::journal::read(&path).with_context(|| {
            format!(
                "cannot read the in-flight journal {} (what the last run had in flight): refusing to start; fix the file's permissions, or move it away once no transaction of this executor is pending",
                path.display()
            )
        })?;
    }
    tracing::info!(
        network = %cfg.network,
        rpc = %crate::rpc::redact_url(cfg.primary_url()),
        data = %cfg.data_dir.display(),
        operator = %kob_protocol::json::to_hex(&signer.pubkey()),
        funding_address = %signer.address(&cfg.network),
        matcher = run_cfg.roles.matcher,
        keeper = run_cfg.roles.keeper,
        dry_run = run_cfg.dry_run,
        fee_floor = fee_policy.floor,
        fee_estimate = fee_policy.dynamic,
        fee_max_rate = fee_policy.max_rate,
        fee_max_tx_sompi = fee_policy.max_tx_fee,
        "starting the executor (indexer, matcher and keepers in one process)"
    );
    if a.dry_run && a.x402_config.is_some() {
        tracing::warn!("dry run: the x402 facilitator is not served (it would settle payers' transactions on the network)");
    }
    let mut fast = crate::faststart::plan(&cfg);
    if fast != crate::faststart::Plan::Normal && a.x402().is_some() {
        // the facilitator's acceptance tracking and book are bound to one store
        tracing::warn!("no fast start with the x402 facilitator: the indexer first catches up with the chain (up to some hours on a new data directory); the matcher and keepers plan nothing until it has");
        fast = crate::faststart::Plan::Normal;
    }
    if fast != crate::faststart::Plan::Normal {
        let full_chain = node_client(&cfg);
        let tip_chain = node_client(&crate::faststart::tip_config(&cfg));
        let node = multi_submit(&cfg, &full_chain);
        return crate::faststart::run_with(cfg, fast, full_chain, tip_chain, node, Box::new(signer), run_cfg, shutdown).await;
    }
    let indexer = Indexer::open(cfg.clone()).map_err(|e: IndexerError| anyhow::anyhow!(e))?;
    let x402 = match a.x402() {
        Some(p) => {
            let ingest = indexer.ingest.clone();
            let c = cfg.clone();
            let p = p.to_path_buf();
            let pause = a.pause_file.clone();
            let health = indexer.health.clone();
            Some(tokio::task::block_in_place(move || x402_service(&p, &c, ingest, pause, Some(health)))?)
        }
        None => None,
    };
    let chain = node_client(&cfg);
    let node = multi_submit(&cfg, &chain);
    run_with_x402(indexer, chain, node, Box::new(signer), run_cfg, x402, shutdown).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Audit M-10: a key file inside the data directory is refused; a pause file there is refused when the data directory is
    /// gone (a pause set there went with it) and only warned about while it exists; files next to it pass. `..` and `.` are
    /// resolved before the comparison, and a sibling whose name starts with the directory's is not inside it.
    #[test]
    fn a_key_or_pause_file_inside_the_data_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("kob-data");
        let key = data.join("kob-operator.key");
        let e = refuse_files_in_data_dir(Some(&key), None, &data).unwrap_err().to_string();
        assert!(e.contains("key file") && e.contains("key_file = \"kob-operator.key\""), "{e}");
        let pause = data.join("pause");
        let e = refuse_files_in_data_dir(None, Some(&pause), &data).unwrap_err().to_string();
        assert!(e.contains("does not exist") && e.contains("pause_file = \"kob-pause\""), "{e}");
        std::fs::create_dir(&data).unwrap();
        assert!(refuse_files_in_data_dir(None, Some(&pause), &data).is_ok(), "warned about while the directory exists");
        // next to it, or a sibling with a longer name: outside
        let next = dir.path().join("kob-pause");
        assert!(refuse_files_in_data_dir(Some(&dir.path().join("kob-operator.key")), Some(&next), &data).is_ok());
        assert!(!inside(&dir.path().join("kob-data2").join("k"), &data));
        assert!(inside(&dir.path().join("x").join("..").join("kob-data").join(".").join("k"), &data));
        assert!(!inside(&data, &data), "the directory itself is not a file inside it");
    }
}
