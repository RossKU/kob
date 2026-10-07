//! `kob-executor run`: the indexer, the matcher and the keepers in one process over one store.
//!
//! * the follower (VSPC v2, `High`) indexes the chain into SQLite and serves the read API, as
//!   `kob-executor index` does;
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
use anyhow::{bail, Result};
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
}

impl IndexerSource {
    /// A source over the store alone (always ready: the caller keeps the store current).
    pub fn new(ingest: Arc<Mutex<Ingest>>) -> Self {
        IndexerSource { ingest, health: None }
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
        Ok(book::snapshot(self.lock().conn(), Some(*operator))?)
    }

    fn watch(&mut self, txid: [u8; 32]) {
        self.lock().watch_tx(Hash32(txid));
    }

    fn keeper_only_orders(&mut self) -> Vec<crate::matcher::book::ListedOrder> {
        book::unlisted_orders(self.lock().conn()).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "cannot read the unlisted orders from the store; the keepers serve the listed ones");
            vec![]
        })
    }

    fn operator_tokens(&mut self, operator: &[u8; 32]) -> Vec<crate::maintenance::OwnToken> {
        book::operator_tokens(self.lock().conn(), operator).unwrap_or_else(|e| {
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
    }

    fn acceptance(&mut self, tracked: &[[u8; 32]]) -> Option<BTreeMap<[u8; 32], (String, u64)>> {
        let ids: Vec<Hash32> = tracked.iter().map(|t| Hash32(*t)).collect();
        let seen = self.lock().watched_acceptance(&ids);
        Some(seen.into_iter().filter_map(|(t, a)| a.map(|(h, daa)| (t.0, (h.to_hex(), daa)))).collect())
    }

    fn metrics(&mut self) -> String {
        self.health.as_ref().map(|h| h.snapshot().metrics_text()).unwrap_or_default()
    }
}

/// `kob-executor run`.
#[derive(Args, Debug, Clone)]
pub struct RunArgs {
    #[command(flatten)]
    pub index: IndexOpts,
    /// File holding the operator's secret key (64 hex, mode 0600). Not the key itself.
    #[arg(long, env = "KOB_OPERATOR_KEY_FILE")]
    pub key_file: Option<PathBuf>,
    /// Build and validate everything, submit nothing.
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
    /// Kill switch: nothing is built or submitted while this file exists.
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
    /// Also serve the x402 facilitator (strict JSON configuration, `crates/kob-executor/x402.example.json`).
    /// Its `node` is the indexer's `--rpc-url`; its `network` must be the indexer's.
    #[arg(long, env = "KOB_X402_CONFIG")]
    pub x402_config: Option<PathBuf>,
}

impl RunArgs {
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
        RunConfig {
            maintenance,
            network: network.to_string(),
            roles: Roles { matcher: !self.no_match, keeper: !self.no_keep },
            engine,
            keeper: KeeperConfig {
                fee_rate: self.fee_rate,
                refund: !self.no_refund,
                sweep_own_strays: self.sweep_own_strays,
                return_foreign_strays: self.return_foreign_strays,
                min_profit: self.keeper_min_profit,
                ..KeeperConfig::default()
            },
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
    let running = indexer.spawn(chain)?;
    let source = IndexerSource::new(indexer.ingest.clone()).with_health(indexer.health.clone());
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
    res
}

/// The x402 facilitator of `kob-executor run`: the configuration at `path` with the indexer's node
/// and network, over [`IndexedChain`](crate::x402::indexed::IndexedChain) (node RPC + the indexer's
/// acceptance tracking).
pub fn x402_service(
    path: &std::path::Path,
    index: &crate::config::IndexerConfig,
    ingest: Arc<Mutex<Ingest>>,
) -> Result<crate::x402::Service> {
    use crate::x402::config::X402Config;
    let mut xcfg = X402Config::load(path)?;
    xcfg.node = index.rpc_url.clone();
    let built = xcfg.build()?;
    if built.network.registry_name() != index.network {
        bail!("x402: the facilitator is configured for {}, the indexer follows {}", built.network, index.network);
    }
    let node = crate::x402::node_chain(&xcfg, &built).map_err(anyhow::Error::msg)?;
    let book_ingest = ingest.clone();
    let book: crate::x402::BookFn = Arc::new(move || {
        let g = book_ingest.lock().unwrap_or_else(|e| e.into_inner());
        let b = book::snapshot(g.conn(), None).map_err(|e| e.to_string())?;
        Ok(crate::x402::quote::book_view(&b))
    });
    let chain = crate::x402::indexed::IndexedChain::new(node, ingest);
    crate::x402::prepare_with(built, Arc::new(chain), Some(book)).map_err(anyhow::Error::msg)
}

/// `kob-executor run`.
pub async fn run(a: RunArgs) -> Result<()> {
    if a.no_match && a.no_keep {
        bail!("both roles are off: `kob-executor index` is the indexer alone");
    }
    let fee_policy = a.fees.policy(a.fee_rate).map_err(anyhow::Error::msg)?;
    let cfg = build_config(&a.index)?;
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
    }
    if a.max_book_lag_daa.is_none() && cfg.lag_tolerance_daa() > 0 {
        // one bound for both gates: a store within the tolerance is a book the runner may plan against
        run_cfg.max_book_lag = Some(cfg.lag_tolerance_daa());
    }
    tracing::info!(
        network = %cfg.network,
        rpc = %crate::rpc::redact_url(cfg.primary_url()),
        data = %cfg.data_dir.display(),
        operator = %kob_protocol::json::to_hex(&signer.pubkey()),
        matcher = run_cfg.roles.matcher,
        keeper = run_cfg.roles.keeper,
        dry_run = run_cfg.dry_run,
        fee_floor = fee_policy.floor,
        fee_estimate = fee_policy.dynamic,
        fee_max_rate = fee_policy.max_rate,
        fee_max_tx_sompi = fee_policy.max_tx_fee,
        "starting the executor (indexer, matcher and keepers in one process)"
    );
    let indexer = Indexer::open(cfg.clone()).map_err(|e: IndexerError| anyhow::anyhow!(e))?;
    let x402 = match &a.x402_config {
        Some(p) => {
            let ingest = indexer.ingest.clone();
            let c = cfg.clone();
            let p = p.clone();
            Some(tokio::task::block_in_place(move || x402_service(&p, &c, ingest))?)
        }
        None => None,
    };
    let chain = node_client(&cfg);
    // transactions go to every node that takes submissions (the primary's verdict counts), every other call to the primary
    let others = chain
        .specs()
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, s)| s.submit)
        .map(|(i, s)| (i, WrpcNode::new(WrpcConfig::new(s.url.clone()))))
        .collect();
    let node = MultiSubmit::new(WrpcNode::new(WrpcConfig::new(cfg.primary_url().to_string())), others, Some(chain.board()));
    run_with_x402(indexer, chain, node, Box::new(signer), run_cfg, x402, shutdown_signal()).await
}
