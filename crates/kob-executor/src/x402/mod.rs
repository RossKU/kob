//! The x402 facilitator role of `kob-executor`: alone (`kob-executor x402`) or inside `kob-executor run`
//! (`--x402-config`), where it follows acceptance through the indexer.
//!
//! | Module | Contents |
//! |---|---|
//! | [`config`] | strict JSON configuration and its validation |
//! | [`ledger`] | durable replay ledger (JSONL write-ahead log, fsync per record) |
//! | [`node`] | `ChainView` over the node's wRPC JSON |
//! | [`facilitator`] | `supported`, `verify`, `settle`, `reconcile` |
//! | [`http`] | axum service: admission, authentication, routes, server loop |
//! | [`ratelimit`] | token buckets |
//! | [`indexed`] | `ChainView` over the node and the indexer's acceptance tracking (`kob-executor run`) |
//! | [`quote`] | swap-and-pay quotes from an indexer's book; the book view of intent executions |

pub mod config;
pub mod facilitator;
pub mod http;
pub mod indexed;
pub mod ledger;
pub mod node;
pub mod quote;
pub mod ratelimit;
#[doc(hidden)]
pub mod testutil;

#[cfg(test)]
mod facilitator_tests;
#[cfg(test)]
mod http_tests;

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use kob_x402::chain::{ChainView, SystemClock};

use config::{AuthMode, Built, X402Config};
use facilitator::{Facilitator, FacilitatorConfig};
use ledger::Ledger;
use node::NodeChain;

/// `kob-executor x402`: the facilitator alone (without the indexer's acceptance tracking; finality
/// from the UTXO observation). Flags override the configuration file.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct X402Args {
    /// Strict JSON configuration file (`crates/kob-executor/x402.example.json`).
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// `kaspa:testnet-10` or `kaspa:mainnet`.
    #[arg(long)]
    pub network: Option<String>,
    /// Node JSON wRPC endpoint (`ws://host:port`).
    #[arg(long)]
    pub node: Option<String>,
    /// HTTP listen address (`addr:port`).
    #[arg(long)]
    pub listen: Option<String>,
    /// Replay ledger path, or `:memory:` (volatile: replay protection is lost on restart).
    #[arg(long)]
    pub ledger: Option<String>,
    /// Merchant authentication: `required` (Bearer keys from the configuration) or `open` (loopback only).
    #[arg(long, value_parser = ["required", "open"])]
    pub auth: Option<String>,
    /// With `--auth open`: state that nothing on this host relays connections to the listen address (configuration
    /// `openAuthNoProxy`).
    #[arg(long)]
    pub open_auth_no_proxy: bool,
}

impl X402Args {
    /// The configuration file (or the defaults) with the flags applied.
    pub fn config(&self) -> Result<X402Config, String> {
        let mut cfg = match &self.config {
            Some(p) => X402Config::load(p).map_err(|e| e.to_string())?,
            None => X402Config::default(),
        };
        if let Some(v) = &self.network {
            cfg.network = v.clone();
        }
        if let Some(v) = &self.node {
            cfg.node = v.clone();
        }
        if let Some(v) = &self.listen {
            cfg.listen = v.clone();
        }
        if let Some(v) = &self.ledger {
            cfg.ledger = v.clone();
        }
        match self.auth.as_deref() {
            Some("required") => cfg.auth = AuthMode::Required,
            Some("open") => cfg.auth = AuthMode::Open,
            Some(_) => return Err("--auth must be required or open".into()),
            None => {}
        }
        if self.open_auth_no_proxy {
            cfg.open_auth_no_proxy = true;
        }
        Ok(cfg)
    }
}

/// A facilitator ready to serve: validated configuration and the core over its chain view.
pub struct Service {
    pub fac: Arc<Facilitator>,
    pub built: Built,
}

/// The node view of `cfg`, checked at startup: another network or a node without a UTXO index is
/// refused; an unreachable node is only a warning (settlements fail closed until it answers).
pub fn node_chain(cfg: &X402Config, built: &Built) -> Result<NodeChain, String> {
    let chain = NodeChain::connect(&cfg.node, built.network, Duration::from_millis(cfg.node_timeout_ms));
    match chain.check_network() {
        Ok(()) => {}
        Err(node::NetworkCheck::Mismatch(m)) => return Err(m),
        Err(node::NetworkCheck::Unreachable(m)) => {
            tracing::warn!("x402: node not reachable at startup ({m}); settlements fail closed until it is")
        }
    }
    Ok(chain)
}

/// The book intent payments are executed against (inside `kob-executor run`: the indexer's).
pub type BookFn = Arc<dyn Fn() -> Result<kob_x402::intent::BookView, String> + Send + Sync>;

/// Opens the ledger and builds the facilitator over `chain`, then reconciles the ledger once. Without a book,
/// intent payments are refused (`intents.enabled` is an error).
pub fn prepare(built: Built, chain: Arc<dyn ChainView>) -> Result<Service, String> {
    prepare_with(built, chain, None)
}

/// [`prepare`] with the book intent payments are executed against.
pub fn prepare_with(built: Built, chain: Arc<dyn ChainView>, book: Option<BookFn>) -> Result<Service, String> {
    let cfg = &built.cfg;
    let ledger = if cfg.ledger == ":memory:" {
        tracing::warn!("x402: volatile ledger; replay protection is lost on restart");
        Ledger::in_memory()
    } else {
        Ledger::open(&cfg.ledger).map_err(|e| e.to_string())?
    };
    let mut fac = Facilitator::new(
        built.policy.clone(),
        chain,
        Arc::new(SystemClock),
        Arc::new(ledger),
        FacilitatorConfig {
            settle_wait: Duration::from_millis(cfg.settle_wait_ms),
            poll_interval: Duration::from_millis(cfg.poll_interval_ms),
            reorg_watch_daa: cfg.reorg_watch_daa,
            kill_switch_file: cfg.kill_switch_file.clone().map(Into::into),
            pause_file: cfg.pause_file.clone(),
            ..Default::default()
        },
    );
    if cfg.intents.enabled {
        let book =
            book.ok_or("intents need the indexer's book: enable them in kob-executor run (--x402-config), not in kob-executor x402")?;
        let keeper = kob_protocol::registry::parse_hex32(&cfg.intents.keeper_pubkey).ok_or("intents.keeperPubkey")?;
        fac = fac.with_intents(facilitator::IntentRuntime {
            book,
            keeper: kob_x402::intent::KeeperParams {
                keeper,
                filler: cfg.intents.filler_sompi,
                fee: Default::default(),
                max_candidates: cfg.intents.max_candidates,
                max_builds: cfg.intents.max_builds,
            },
            max_attempts: cfg.intents.max_attempts,
            lock_margin_daa: cfg.intents.lock_margin_daa,
        });
        tracing::info!("x402: intent-based swap-and-pay on (router {})", kob_protocol::router::ROUTER_ARTIFACT_ID);
    }
    if cfg.invoices.enabled {
        let path = cfg.invoices.store.clone().unwrap_or_else(|| {
            if cfg.ledger == ":memory:" {
                ":memory:".into()
            } else {
                format!("{}.invoices.jsonl", cfg.ledger)
            }
        });
        let store = if path == ":memory:" {
            tracing::warn!("x402: volatile invoice store; invoices vanish on restart");
            facilitator::InvoiceStore::in_memory()
        } else {
            facilitator::InvoiceStore::open(&path)?
        };
        tracing::info!("x402: invoices on ({} stored)", store.len());
        fac = fac.with_invoices(facilitator::InvoiceRuntime {
            store: Arc::new(store),
            max_lifetime_ms: cfg.invoices.max_lifetime_seconds.saturating_mul(1_000),
            public_url: cfg.invoices.public_url.clone(),
            max_open_per_merchant: cfg.invoices.max_open_per_merchant,
            max_extra_payments: cfg.invoices.max_extra_payments_per_invoice,
        });
    }
    let fac = Arc::new(fac);
    let report = fac.reconcile();
    tracing::info!("x402: ledger has {} entries; startup reconcile: {report:?}", fac.ledger.len());
    Ok(Service { fac, built })
}

impl Service {
    /// Binds the HTTP listen address (before anything else starts, so a taken port fails the start).
    pub async fn bind(&self) -> Result<tokio::net::TcpListener, String> {
        tokio::net::TcpListener::bind(self.built.listen).await.map_err(|e| format!("x402: bind {}: {e}", self.built.listen))
    }
}

/// Serves the facilitator's HTTP API on `listener` and runs the periodic reconcile until `shutdown`
/// resolves.
pub async fn serve_service(svc: Service, listener: tokio::net::TcpListener, shutdown: impl Future<Output = ()>) -> Result<(), String> {
    let Service { fac, built } = svc;
    let cfg = built.cfg.clone();
    tracing::info!(
        "x402: facilitator for {} on http://{} (auth {:?}, {} merchants, node {})",
        built.network,
        built.listen,
        cfg.auth,
        built.merchants.len(),
        crate::rpc::redact_url(&cfg.node)
    );
    let state = Arc::new(http::AppState::new(fac.clone(), &built));
    let app = http::router(state);
    let recon = fac.clone();
    let every = Duration::from_secs(cfg.reconcile_interval_seconds.max(1));
    let reconciler = tokio::spawn(async move {
        loop {
            tokio::time::sleep(every).await;
            let f = recon.clone();
            if let Ok(r) = tokio::task::spawn_blocking(move || f.reconcile()).await {
                if r.reorged > 0 || r.errors > 0 {
                    tracing::warn!("x402: reconcile: {r:?}");
                }
            }
        }
    });
    let res = http::serve(listener, app, cfg.conn_limits(), shutdown).await.map_err(|e| e.to_string());
    reconciler.abort();
    tracing::info!("x402: facilitator stopped");
    res
}

/// `kob-executor x402 ...`: the facilitator alone; returns when the process is asked to stop.
pub async fn run(a: X402Args) -> anyhow::Result<()> {
    let go = async {
        let cfg = a.config()?;
        let built = cfg.build().map_err(|e| e.to_string())?;
        let chain = tokio::task::block_in_place(|| node_chain(&cfg, &built))?;
        let svc = tokio::task::block_in_place(|| prepare(built, Arc::new(chain)))?;
        let listener = svc.bind().await?;
        serve_service(svc, listener, crate::index_cli::shutdown_signal()).await
    };
    go.await.map_err(|e: String| anyhow::anyhow!("x402: {e}"))
}
