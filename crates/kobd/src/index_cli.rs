//! The indexer's command line: the options of a start (`kobd --indexer`, and the set `kobd --executor` shares), the
//! database tools, `kobd db <tool>` (migrate, vacuum, replay, export-orders, export-book, import-orders, rebase), and the token-logo
//! reports, `kobd logo-reports <command>`.

use crate::config::NodeConfig;
use crate::config::{IndexerConfig, StartMode};
use crate::indexer::{self, Indexer};
use crate::recover::{self, MakerExport};
use crate::rpc::failover::Failover;
use crate::rpc::multi::{MultiNode, NodeSpec};
use crate::rpc::{redact_url, ChainSource, WrpcClient, WrpcConfig};
use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;

/// `kobd db <tool>`.
#[derive(Args)]
pub struct DbArgs {
    #[command(flatten)]
    pub store: StoreOpts,
    #[command(subcommand)]
    pub sub: DbSub,
}

/// `kobd logo-reports <command>`.
#[derive(Args)]
pub struct LogoReportsArgs {
    #[command(flatten)]
    pub store: StoreOpts,
    #[command(subcommand)]
    pub cmd: LogoCmd,
}

/// The configuration and the store: what every start and every database tool takes.
#[derive(Args, Clone, Debug, Default)]
#[command(next_help_heading = "Configuration and store")]
pub struct StoreOpts {
    /// TOML configuration file (all keys optional); flags override it.
    #[arg(long, global = true, env = "KOB_INDEX_CONFIG")]
    pub config: Option<PathBuf>,
    /// Network id the node must report (`mainnet`, `testnet-10`). Required on the reference build (no default); a deployment
    /// build (`deploy-mainnet`, `deploy-tn10`) implies its own and refuses another.
    #[arg(long, global = true, env = "KOB_NETWORK")]
    pub network: Option<String>,
    /// Directory holding the SQLite database and the record log (`data_dir`; default `kob-data`).
    #[arg(long, global = true, env = "KOB_INDEX_DATA_DIR")]
    pub data_dir: Option<PathBuf>,
    /// Token allowlist JSON (`tokens_path`; `registry/tokens.json`).
    #[arg(long, global = true)]
    pub tokens: Option<PathBuf>,
}

/// The node and the further nodes: a start, and the database tools that ask a node (`import-orders`, `rebase`).
#[derive(Args, Clone, Debug, Default)]
#[command(next_help_heading = "Node")]
pub struct NodeOpts {
    /// THE node (the chain authority): JSON wRPC (`ws://`, `wss://`), Borsh wRPC (a `.../borsh` path or a default Borsh port
    /// `17110` / `17210`: every call in Borsh), or `resolver`: a public Kaspa node of the network found through the Kaspa
    /// resolvers (Borsh over TLS, always a cross-checked pair; another one after a failure). Default: `resolver`. An own node
    /// (the local one listens on `ws://127.0.0.1:18110` on mainnet, `ws://127.0.0.1:18210` on testnet-10) is named here. A URL
    /// with a password or an API key belongs in the config file (`rpc_url`) or in `KOB_NODE_WRPC`, not on the command line
    /// (`ps`).
    #[arg(long, global = true, env = "KOB_NODE_WRPC", value_name = "URL")]
    pub rpc_url: Option<String>,
    /// A further node beside the node itself (`ws://` or `wss://`, JSON or Borsh wRPC as `--rpc-url`, or `resolver`: another
    /// public node, resolved independently of the primary's; repeatable): windows of transaction bodies are fetched from it
    /// too (checked against the node, `--rpc-url`) and transactions submitted to it too. Added to the config's `[[nodes]]`.
    /// A URL with a password or an API key belongs in the config file or in `KOB_INDEX_NODES` (comma-separated), not on the
    /// command line (`ps`).
    #[arg(long = "extra-node", global = true, value_name = "URL", env = "KOB_INDEX_NODES", value_delimiter = ',')]
    pub nodes: Vec<String>,
    /// The option of earlier builds for a further node: refused with the new name (`--extra-node`), never taken as the node.
    #[arg(long = "node", global = true, value_name = "URL", hide = true)]
    pub old_node: Vec<String>,
    /// With `--extra-node`: the node itself serves windows of transaction bodies only when no other node can (it stays the
    /// chain authority).
    #[arg(long, global = true)]
    pub no_primary_fetch: bool,
    /// Fetch the windows of transaction bodies over each node's Borsh wRPC endpoint (`borsh` in the config).
    #[arg(long, global = true)]
    pub borsh: bool,
    /// Parallel windows while the cursor is more than this many blue score behind the sink (`prefetch_min_lag_blue`).
    #[arg(long, global = true, value_name = "BLUE")]
    pub prefetch_min_lag_blue: Option<u64>,
}

/// The refusal of the option `--node` of earlier builds on a start and the database tools: since this release a further
/// node is `--extra-node`, and the node itself has always been `--rpc-url` (`kobd setup --node`, `kobd x402 --node` and `kob
/// --node` name the node, so the word would otherwise mean two things).
pub const OLD_NODE_OPTION: &str =
    "--node is now --extra-node for a second node; the node itself is --rpc-url (or rpc_url in kobd.toml)";

impl NodeOpts {
    /// Refuses the option of earlier builds ([`OLD_NODE_OPTION`]).
    pub fn refuse_old_node(&self) -> Result<()> {
        if self.old_node.is_empty() {
            Ok(())
        } else {
            Err(crate::cli::Usage(OLD_NODE_OPTION.to_string()).into())
        }
    }
}

/// The indexer options every start takes (`kobd --indexer`, `kobd --executor`).
#[derive(Args, Clone, Debug, Default)]
pub struct IndexOpts {
    #[command(flatten)]
    pub store: StoreOpts,
    #[command(flatten)]
    pub node: NodeOpts,
    /// Where a fresh database starts: `pruning-point` (default), `sink`, or a chain block hash.
    #[arg(long, global = true, help_heading = "Storage", value_name = "WHERE")]
    pub start: Option<StartMode>,
    /// The Executor on an empty data directory: trade at once on a store that follows the chain from the node's sink, and index the
    /// history into the normal store meanwhile (`fast_start` in the configuration; docs/ops/executor.md, *Fast start*). On by
    /// default for the Executor.
    #[arg(long, global = true, conflicts_with = "no_fast_start", help_heading = "Storage")]
    pub fast_start: bool,
    /// Turn fast start off for this start: the history is indexed before the matcher plans (fast start is on for the Executor
    /// unless the configuration says `fast_start = false`).
    #[arg(long, global = true, help_heading = "Storage")]
    pub no_fast_start: bool,
    /// Lean storage: keep the live book and 30 days of closed orders (500 MB cap), no record log, a file that shrinks (`lean` in the
    /// configuration; docs/ops/executor.md, *Lean storage*). The Executor alone is always lean, the Indexer never.
    #[arg(long, global = true, conflicts_with = "no_lean", help_heading = "Storage")]
    pub lean: bool,
    /// Turn `lean` of the configuration off for this start.
    #[arg(long, global = true, help_heading = "Storage")]
    pub no_lean: bool,
    /// Lean storage: days closed orders stay readable (`lean_retention_days`, default 30).
    #[arg(long, global = true, help_heading = "Storage", value_name = "DAYS")]
    pub lean_retention_days: Option<u64>,
    /// Lean storage: size cap of the database in MB (`lean_max_db_mb`, default 500; 0: none).
    #[arg(long, global = true, help_heading = "Storage", value_name = "MB")]
    pub lean_max_db_mb: Option<u64>,
    /// Disable the permanent record log (not recommended: it is the rebuild source, see docs/ops/executor.md, Part B).
    #[arg(long, global = true, help_heading = "Storage")]
    pub no_record_log: bool,
    /// Hours of chain blocks kept for reorgs (the finality window; default 12).
    #[arg(long, global = true, help_heading = "Storage", value_name = "HOURS")]
    pub reorg_window_hours: Option<u64>,
    /// Read API listen address (`[api] listen`; default `127.0.0.1:8090`).
    #[arg(long, global = true, help_heading = "Read API", value_name = "ADDR")]
    pub listen: Option<std::net::SocketAddr>,
    /// Serve `GET /v1/price/kas-usd`, the KAS price in USD for the web app's optional USD display (`usd_price` in `[api]`). The
    /// indexer asks CoinGecko's keyless API for it (a KuCoin KAS-USDT ticker when CoinGecko fails), at most once a minute
    /// (backing off to 10 minutes while an upstream fails) and only while a client asks, and serves the last good value with
    /// its time and source. Off by default: no call to a third party unless this is given.
    #[arg(long, global = true, env = "KOB_USD_PRICE", help_heading = "Read API")]
    pub usd_price: bool,
    /// Do not serve the read API.
    #[arg(long, global = true, help_heading = "Read API")]
    pub no_api: bool,
    /// Log a one-line status (the indexer's lag; with the Executor also the transactions sent, the profit, the funds and the
    /// warnings) every this many seconds, as kaspad logs one every 10 s; 0: never.
    #[arg(long, global = true, default_value_t = 10, help_heading = "Log", value_name = "SECS")]
    pub status_secs: u64,
}

impl IndexOpts {
    /// `--status-secs` as a period (`None`: off).
    pub fn status_every(&self) -> Option<std::time::Duration> {
        (self.status_secs > 0).then(|| std::time::Duration::from_secs(self.status_secs))
    }

    /// The configuration file (`--config`).
    pub fn config(&self) -> Option<&std::path::Path> {
        self.store.config.as_deref()
    }

    /// The options of a database tool: the store's, with the node's when the tool asks a node.
    pub fn of_store(store: &StoreOpts, node: Option<&NodeOpts>) -> IndexOpts {
        IndexOpts { store: store.clone(), node: node.cloned().unwrap_or_default(), ..IndexOpts::default() }
    }
}

/// The schema numbers of `kobd db migrate --help`, from the code: the lowest schema it brings forward and this build's.
fn migrate_about() -> &'static str {
    static ABOUT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ABOUT.get_or_init(|| {
        format!(
            "Bring a data directory written by an earlier build (schema {} to {}) to this build's layout (schema {}) in one run: a \
             new store is built beside the old one (integer ids, packed state spans, fills-only indexes), verified row by row \
             against it and swapped in, and the sealed record-log segments are gzip-compressed. Stop the executor first (it takes \
             the writer lock); this build refuses an older store until this has run. Safe to repeat or to stop (the old store is \
             not touched before the swap); the previous store file is kept beside the new one (`index.sqlite3.before-migrate`) \
             until you delete it. Needs free disk for the new store (about three quarters of the old one) and the temporary files \
             of the index builds.",
            crate::indexer::storage::MIGRATE_FROM_SCHEMA,
            crate::indexer::db::SCHEMA_VERSION - 1,
            crate::indexer::db::SCHEMA_VERSION
        )
    })
}
#[derive(Subcommand)]
pub enum DbSub {
    /// Rebuild a NEW database from the record log (the log is not modified), then follow the node from its cursor.
    Replay {
        /// Bring the EXISTING database forward instead: apply the record-log frames it has not committed (a database
        /// restored from a backup next to a newer log). The log is not modified.
        #[arg(long)]
        onto_database: bool,
    },
    #[command(
        about = "Bring a data directory of an earlier build to this build's schema (offline; stop the executor first)",
        long_about = migrate_about()
    )]
    Migrate {
        /// Do not compress the sealed record-log segments.
        #[arg(long)]
        no_compress_log: bool,
    },
    /// Give the free pages of the store back to the file system (offline; stop the executor first): a copy without them is checked and
    /// takes the place of the store, whose file is deleted. Once after the first pruning of a store that kept its whole history (the
    /// archive pruning frees most of it; docs/ops/executor.md 7.6b). Needs free disk for the copy (the pages in use).
    Vacuum,
    /// Print a maker order export (JSON) from the database.
    ExportOrders {
        /// Maker public key (hex); default: all makers.
        #[arg(long)]
        maker: Option<String>,
        /// Only open and partially filled orders.
        #[arg(long)]
        live_only: bool,
    },
    /// Write the matcher's book snapshot (JSON, `MemoryBook`: every listed order with its custody and strays, and the
    /// keeper-only orders, at the store's cursor) for `match` / `keep --book-file`. Read-only next to a running writer;
    /// `--out` replaces the file atomically.
    ExportBook {
        /// Output file (written to a temporary sibling, then renamed); default: standard output.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Recovery hook: import orders from a maker export or archive dump, verified against the node's UTXO set.
    ImportOrders {
        file: PathBuf,
        #[command(flatten)]
        node: NodeOpts,
    },
    /// After a gap: move the cursor to a fresh start point and reconcile the tracked orders with the node.
    Rebase {
        /// `sink` or a chain block hash.
        #[arg(long, default_value = "sink")]
        to: StartMode,
        /// Only move the cursor; do not touch the tracked orders (they may stay open although spent).
        #[arg(long)]
        no_reconcile: bool,
        #[command(flatten)]
        node: NodeOpts,
    },
    /// What a data directory holds, read-only and offline (works next to a running executor): the schema, the network, the
    /// cursor, the orders, the sizes, lean or archive storage, where the history begins.
    Info {
        /// One JSON object instead of the text.
        #[arg(long)]
        json: bool,
    },
}

/// `kobd logo-reports`.
#[derive(Subcommand)]
pub enum LogoCmd {
    /// The listed tokens, the hidden and not yet reviewed ones first, with the reports about each.
    List {
        /// One JSON array instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Show the logo again. New reports for the token are then only recorded for `cooldown_secs` (default 6 hours), after that they hide it again.
    Restore {
        /// Covenant id (64 hex characters).
        covenant_id: String,
        /// Also restore a token that was hidden for good (`hide`).
        #[arg(long)]
        force: bool,
    },
    /// Hide the logo for good: reports never restore it; `restore --force` does. The token need not have been reported.
    Hide {
        /// Covenant id (64 hex characters).
        covenant_id: String,
    },
    /// Print the whole report state as JSON (reports are not derived from the chain: export before `kobd db replay` rebuilds the database).
    Export,
    /// Load an export into an empty report store (after `kobd db replay`).
    Import { file: PathBuf },
}

/// The relative paths of a configuration file (`data_dir`, `tokens_path`, `[records] dir`) are relative to the file's
/// directory, as `log_file` is (`crate::cli`), so `--config <dir>/kobd.toml` started from any directory reads the
/// same files as a start in `<dir>`. A file named without a directory (the launcher's `--config kobd.toml`, run in
/// the file's directory) keeps its paths as written. Paths given as flags stay relative to the working directory.
fn relative_to_config(cfg: &mut IndexerConfig, file: &std::path::Path) {
    let Some(base) = file.parent().filter(|d| !d.as_os_str().is_empty()) else { return };
    cfg.data_dir = crate::logging::resolve(base, &cfg.data_dir);
    for p in [cfg.tokens_path.as_mut(), cfg.records.dir.as_mut()].into_iter().flatten() {
        *p = crate::logging::resolve(base, p);
    }
}

pub fn build_config(a: &IndexOpts) -> Result<IndexerConfig> {
    a.node.refuse_old_node()?;
    let (s, n) = (&a.store, &a.node);
    let mut cfg = match &s.config {
        Some(p) => {
            let mut c = IndexerConfig::load(p)?;
            relative_to_config(&mut c, p);
            c
        }
        None => IndexerConfig::default(),
    };
    if let Some(v) = &s.network {
        cfg.network = v.clone();
    }
    if let Some(v) = &n.rpc_url {
        cfg.rpc_url = v.clone();
    }
    if n.no_primary_fetch {
        cfg.primary_fetch = false;
    }
    if let Some(v) = n.prefetch_min_lag_blue {
        cfg.prefetch_min_lag_blue = v;
    }
    if n.borsh {
        cfg.borsh = true;
    }
    for url in &n.nodes {
        if !cfg.nodes.iter().any(|x| &x.url == url) {
            cfg.nodes.push(NodeConfig { url: url.clone(), ..NodeConfig::default() });
        }
    }
    if let Some(v) = &s.data_dir {
        cfg.data_dir = v.clone();
    }
    if let Some(v) = &s.tokens {
        cfg.tokens_path = Some(v.clone());
    }
    if let Some(v) = &a.start {
        cfg.start = v.clone();
    }
    if let Some(v) = a.listen {
        cfg.api.listen = v;
    }
    if a.fast_start {
        cfg.fast_start = true;
    }
    if a.no_fast_start {
        cfg.fast_start = false;
    }
    if a.lean {
        cfg.lean = true;
    }
    if a.no_lean {
        cfg.lean = false;
    }
    if let Some(v) = a.lean_retention_days {
        cfg.lean_retention_days = v;
    }
    if let Some(v) = a.lean_max_db_mb {
        cfg.lean_max_db_mb = v;
    }
    if a.usd_price {
        cfg.api.usd_price = true;
    }
    if a.no_api {
        cfg.api.enabled = false;
    }
    if a.no_record_log {
        cfg.records.enabled = false;
    }
    // lean storage keeps no record log (it is the rebuild source of an archive; a lean store is rebuilt from the chain's
    // retention in a day)
    if cfg.lean {
        cfg.records.enabled = false;
    }
    if let Some(v) = a.reorg_window_hours {
        cfg.reorg_window_hours = v;
    }
    // A deployment build (e.g. `--features deploy-tn10`) records the network it was built for (the templates are the
    // reference ones; the build only refuses to index another network). The reference build implies none: the network
    // is given explicitly, never a silent testnet-10.
    cfg.network = crate::config::resolve_network(Some(&cfg.network)).map_err(|e| anyhow!(e))?;
    if cfg.rpc_url.trim().is_empty() {
        // no node named: the public pair (an own node is named with its URL)
        cfg.rpc_url = crate::config::default_node(&cfg.network).ok_or_else(|| {
            anyhow!("no node endpoint for network {}: pass --rpc-url (or `rpc_url` in the configuration)", cfg.network)
        })?;
    }
    number_resolver_entries(&mut cfg);
    check_failover(&cfg)?;
    // the resolver entries resolve public nodes of this network
    crate::rpc::resolver::set_network(&cfg.network);
    Ok(cfg)
}

/// The `[failover]` table, refused rather than half used: the fallback is the public pair (`resolver`), which the configured
/// nodes must not use already (the pair's entries are process-wide, `rpc::resolver`).
pub fn check_failover(cfg: &IndexerConfig) -> Result<()> {
    use crate::rpc::resolver::{is_resolver, RESOLVER};
    let f = &cfg.failover;
    if !f.enabled() {
        return Ok(());
    }
    if f.fallback.trim() != RESOLVER {
        anyhow::bail!(
            "[failover] fallback = {:?}: the only fallback is \"resolver\" (the public Kaspa nodes as a cross-checked pair)",
            f.fallback
        );
    }
    if is_resolver(cfg.primary_url()) || cfg.nodes.iter().any(|n| is_resolver(&n.url)) {
        anyhow::bail!(
            "[failover] fallback = \"resolver\" is for a configured node (rpc_url = \"ws://...\"): this configuration uses the public nodes already (rpc_url or nodes = \"resolver\"), which fail over among themselves; remove the [failover] table"
        );
    }
    Ok(())
}

/// Gives every resolver entry of the configuration its own pseudo URL (`rpc::resolver::pseudo`), so each resolves a public
/// node of its own on another host: the primary's `resolver` stays `resolver`; a primary `resolver` always comes as a pair, the
/// second entry `resolver:2` (added first, before any other node), which fetches windows, takes every submission and vouches
/// for the primary's answers (`rpc::multi`, `NodeSpec::cross_check`); every other node `resolver` (`--extra-node resolver`) becomes
/// the next number.
pub fn number_resolver_entries(cfg: &mut IndexerConfig) {
    use crate::config::NodeRole;
    use crate::rpc::resolver::{pseudo, RESOLVER};
    let mut n = 2;
    if cfg.primary_url() == RESOLVER {
        cfg.nodes.insert(0, NodeConfig { url: pseudo(2), ..NodeConfig::default() });
        n = 3;
    }
    for node in cfg.nodes.iter_mut().filter(|x| x.url == RESOLVER && x.role == NodeRole::Secondary) {
        node.url = pseudo(n);
        n += 1;
    }
}

fn wrpc(cfg: &IndexerConfig, url: &str, vspc_connections: usize) -> Arc<WrpcClient> {
    let mut w = WrpcConfig::new(url.to_string());
    w.request_timeout = std::time::Duration::from_secs(cfg.rpc_timeout_secs);
    w.vspc_connections = vspc_connections;
    // Borsh only on VSPC connections of its own (with none, VSPC shares the JSON connection)
    w.borsh_url = cfg.borsh_url_of(url).filter(|_| vspc_connections > 0);
    if let Some(b) = &w.borsh_url {
        tracing::info!(node = %redact_url(url), borsh = %redact_url(b), connections = vspc_connections, "windows of transaction bodies over Borsh wRPC");
    }
    if cfg.borsh && w.borsh_url.is_none() && vspc_connections > 0 && !crate::rpc::all_borsh(url) {
        tracing::warn!(node = %redact_url(url), "no Borsh endpoint known for this node: its windows stay JSON (set borsh_url)");
    }
    Arc::new(WrpcClient::new(w))
}

/// The node source of the indexer: the primary (`rpc_url`, or the `nodes` entry with `role = "primary"`) and the other
/// nodes of `nodes` (`rpc::multi`). With no other node it behaves exactly as the primary alone.
pub fn node_client(cfg: &IndexerConfig) -> Arc<MultiNode<WrpcClient>> {
    let parallel = cfg.fetch_parallel.max(1);
    // the follower fetches that many VSPC windows at once while it catches up, one connection each
    let conns = |n: usize| if parallel > 1 { n } else { 0 };
    let primary_url = cfg.primary_url().to_string();
    let primary = wrpc(cfg, &primary_url, conns(parallel));
    let primary_spec =
        NodeSpec { url: primary_url.clone(), fetch: cfg.primary_fetches(), submit: true, connections: parallel, cross_check: false };
    // the second node of a resolver pair vouches for the primary's own answers too (rpc::multi)
    let pair = crate::rpc::resolver::is_resolver(&primary_url);
    let mut nodes = vec![(primary_spec, primary.clone())];
    for n in cfg.secondaries() {
        let c = n.connections.unwrap_or(parallel).max(1);
        let cross_check = pair && crate::rpc::resolver::is_resolver(&n.url);
        let spec = NodeSpec { url: n.url.clone(), fetch: n.fetch && parallel > 1, submit: n.submit, connections: c, cross_check };
        nodes.push((spec, wrpc(cfg, &n.url, conns(c))));
    }
    // the acceptance data and the primary's copies of chain blocks that matter, on connections of their own
    let verifier = if nodes.len() > 1 { wrpc(cfg, &primary_url, 4) } else { primary };
    if nodes.len() > 1 {
        tracing::info!(
            primary = %redact_url(&primary_url),
            others = ?nodes[1..].iter().map(|(s, _)| redact_url(&s.url)).collect::<Vec<_>>(),
            windows = cfg.fetch_windows(),
            "several nodes: bodies from all (checked against the primary), the chain from the primary"
        );
    }
    Arc::new(MultiNode::new(nodes, verifier))
}

/// The fallback of the node failover: the public Kaspa nodes as a cross-checked pair (`rpc_url = "resolver"`, with the
/// configuration's fetch settings).
pub fn fallback_client(cfg: &IndexerConfig) -> Arc<MultiNode<WrpcClient>> {
    let mut f = cfg.clone();
    f.rpc_url = crate::rpc::resolver::RESOLVER.to_string();
    f.nodes = vec![];
    f.primary_fetch = true;
    number_resolver_entries(&mut f);
    node_client(&f)
}

/// The Indexer's node source with `[failover]`: the configured nodes ([`node_client`]) first, the public pair
/// ([`fallback_client`]) when they stop answering (`rpc::failover`).
pub fn failover_client(cfg: &IndexerConfig) -> Arc<Failover<MultiNode<WrpcClient>>> {
    use crate::rpc::failover::FailoverPolicy;
    let (primary, fallback) = (node_client(cfg), fallback_client(cfg));
    let policy = FailoverPolicy::from_config(&cfg.failover);
    tracing::info!(
        primary = %redact_url(cfg.primary_url()),
        fallback = %cfg.failover.fallback,
        after_failures = policy.after_failures,
        after_secs = policy.after.as_secs(),
        probe_secs = policy.probe_every.as_secs(),
        "node failover: the configured node first, the public Kaspa nodes (a cross-checked pair) when it stops answering"
    );
    Arc::new(Failover::new(primary, fallback, cfg.primary_url(), &cfg.failover.fallback, &cfg.network, policy))
}

pub use crate::shutdown::shutdown_signal;

/// What the Indexer (`kobd --indexer`) refuses instead of ignoring: the options of role `run` that its configuration sets.
/// The launcher refuses the same `[launch]` keys (`setup::launch_args`); `fast_start` needs the matcher the index role does not have.
/// `file`: the configuration file, for the message.
pub fn refuse_run_only_options(cfg: &IndexerConfig, file: Option<&std::path::Path>) -> Result<()> {
    let at = file.map(|p| format!("{}: ", p.display())).unwrap_or_default();
    if let Some(k) = cfg.launch.as_ref().and_then(|l| l.run_only_key()) {
        anyhow::bail!(
            "{at}[launch] {k} is an option of role \"run\", not of the Indexer (`kobd --indexer` has no key, no kill switch and no \
             x402 facilitator): remove it, or start the Executor (`kobd --executor`)"
        );
    }
    if cfg.fast_start {
        anyhow::bail!(
            "{at}fast_start is an option of the Executor (`kobd --executor`: it lets the matcher trade at once on a store that follows \
             the chain from the node's sink while the history is indexed); the Indexer has no matcher. To follow at once, set `start = \"sink\"` (--start sink): \
             orders older than the start are then not in the store and /v1/health reports book_complete false. Otherwise let it catch up: \
             the status line and /v1/health show the time to go"
        );
    }
    Ok(())
}

/// The tools of `kobd db`, as named on the command line.
/// The Indexer: follow the node and serve the read API (`kobd --indexer`).
pub async fn follow_and_serve(opts: IndexOpts) -> Result<()> {
    let cfg = build_config(&opts)?;
    refuse_run_only_options(&cfg, opts.config())?;
    tracing::info!(network = %cfg.network, rpc = %redact_url(cfg.primary_url()), data = %cfg.data_dir.display(), "starting indexer");
    // listening before the slow part: a Ctrl+C while the store opens is not lost
    let shutdown = shutdown_signal();
    let idx = Indexer::open(cfg)?;
    let status = opts.status_every().map(|every| {
        let health = idx.health.clone();
        tokio::spawn(async move {
            let mut t = tokio::time::interval(every);
            t.tick().await;
            loop {
                t.tick().await;
                tracing::info!("status: {}", health.snapshot().status_line());
            }
        })
    });
    let r = idx.run(shutdown).await;
    if let Some(s) = status {
        s.abort();
    }
    r?;
    Ok(())
}

/// Runs a tool of `kobd db`.
pub async fn run_db(a: DbArgs) -> Result<()> {
    let node = match &a.sub {
        DbSub::ImportOrders { node, .. } | DbSub::Rebase { node, .. } => Some(node),
        _ => None,
    };
    let opts = IndexOpts::of_store(&a.store, node);
    if let DbSub::Info { json } = a.sub {
        return db_info(&opts, json);
    }
    let cfg = build_config(&opts)?;
    match a.sub {
        DbSub::Info { .. } => unreachable!("answered above"),
        DbSub::Replay { onto_database: true } => {
            let f = indexer::replay_onto_database(&cfg)?;
            let r = &f.replay;
            println!(
                "applied record-log frames {}..{} ({} relevant transactions) to {}: {} orders",
                f.from,
                r.frames,
                r.relevant,
                cfg.db_path().display(),
                r.orders
            );
            if !r.skipped.is_empty() {
                println!(
                    "skipped {} frame(s) this build cannot decode {:?}",
                    r.skipped.len(),
                    r.skipped.iter().map(|(n, _)| n).collect::<Vec<_>>()
                );
            }
        }
        DbSub::Replay { onto_database: false } => {
            let r = indexer::replay_from_log(&cfg)?;
            println!(
                "replayed {} record-log frames ({} relevant transactions) into {}: {} orders",
                r.frames,
                r.relevant,
                cfg.db_path().display(),
                r.orders
            );
            if !r.skipped.is_empty() || r.dropped.dropped() > 0 {
                println!(
                    "skipped {} frame(s) this build cannot decode {:?}; dropped {} reveal(s), {} holding(s), {} import(s) of template layouts this build does not have",
                    r.skipped.len(),
                    r.skipped.iter().map(|(n, _)| n).collect::<Vec<_>>(),
                    r.dropped.unknown_reveals,
                    r.dropped.unknown_holds,
                    r.dropped.unknown_imports
                );
            }
            // a placement this build cannot interpret (an order or a token program it no longer pins) is a reject in the new
            // database, not an order: say how many, by reason, so a rebuild that leaves orders out never looks complete
            let conn = indexer::db::open_reader(&cfg.db_path(), &cfg.network)?;
            let mut q = conn.prepare("SELECT reason, COUNT(*) FROM rejects GROUP BY reason ORDER BY 2 DESC LIMIT 10")?;
            let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
            if !rows.is_empty() {
                println!("rejected transactions in the rebuilt database, by reason (the 10 most frequent):");
                for (reason, n) in rows {
                    println!("  {n:>9}  {reason}");
                }
            }
        }
        DbSub::Migrate { no_compress_log } => {
            let opts = indexer::storage::Options { compress_log: !no_compress_log };
            let r = indexer::storage::migrate(&cfg, opts, |line| println!("{line}"))?;
            println!(
                "schema {} -> {}: store {} -> {} bytes; record log {} -> {} bytes ({} segment(s) compressed); rows: {}",
                r.from_schema,
                indexer::db::SCHEMA_VERSION,
                r.db_bytes_before,
                r.db_bytes_after,
                r.log_bytes_before,
                r.log_bytes_after,
                r.log_segments_compressed,
                r.tables.iter().map(|t| format!("{} {}", t.table, t.rows)).collect::<Vec<_>>().join(", ")
            );
            if let Some(old) = r.kept_old {
                println!("the previous store file is kept as {}: delete it once the executor has run on the new one", old.display());
            }
        }
        DbSub::Vacuum => {
            let r = indexer::storage::vacuum(&cfg, |line| println!("{line}"))?;
            println!(
                "store {} -> {} bytes ({} free pages of {} bytes given back)",
                r.bytes_before, r.bytes_after, r.free_pages, r.page_size
            );
        }
        DbSub::ExportOrders { maker, live_only } => {
            // read-only: allowed next to a running writer of the same data directory (nothing indexed yet: an empty export)
            let conn = if cfg.db_path().exists() {
                indexer::db::open_reader(&cfg.db_path(), &cfg.network)?
            } else {
                indexer::db::open_memory(&cfg.network)?
            };
            let maker = maker.map(|m| crate::hex::decode(&m)).transpose()?;
            let export = recover::export_orders(&conn, maker.as_deref(), live_only)?;
            println!("{}", serde_json::to_string_pretty(&export)?);
        }
        DbSub::ExportBook { out } => {
            // read-only, like export-orders (nothing indexed yet: an empty book)
            let conn = if cfg.db_path().exists() {
                indexer::db::open_reader(&cfg.db_path(), &cfg.network)?
            } else {
                indexer::db::open_memory(&cfg.network)?
            };
            let json = serde_json::to_string(&indexer::book::snapshot(&conn, None)?)?;
            match out {
                None => println!("{json}"),
                Some(path) => {
                    // a matcher re-reading the file never sees half of it
                    let mut tmp = path.clone().into_os_string();
                    tmp.push(".tmp");
                    std::fs::write(&tmp, json).with_context(|| PathBuf::from(&tmp).display().to_string())?;
                    std::fs::rename(&tmp, &path).with_context(|| path.display().to_string())?;
                }
            }
        }
        DbSub::ImportOrders { file, .. } => {
            let export: MakerExport =
                serde_json::from_str(&std::fs::read_to_string(&file).with_context(|| file.display().to_string())?)?;
            let idx = Indexer::open(cfg.clone())?;
            let client = node_client(&cfg);
            let info = client.server_info().await?;
            if !info.has_utxo_index {
                return Err(anyhow!("the node runs without --utxoindex; importing needs getUtxosByAddresses"));
            }
            let report = recover::import_orders(&idx.ingest, client.as_ref(), &cfg.network, &export).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        DbSub::Rebase { to, no_reconcile, .. } => {
            let idx = Indexer::open(cfg.clone())?;
            let client = node_client(&cfg);
            let rep = recover::rebase(&idx.ingest, client.as_ref(), &cfg.network, &to, !no_reconcile).await?;
            let (hash, daa) = (rep.cursor.hash, rep.cursor.daa);
            match rep.reconciled {
                None => println!("cursor moved to {hash} (DAA {daa}); tracked orders were NOT reconciled"),
                Some(r) => println!(
                    "cursor moved to {hash} (DAA {daa}); closed {} spent outputs, adopted {} continuations",
                    r.closed, r.adopted
                ),
            }
        }
    }
    Ok(())
}

/// The size of a file and its `-wal` beside it, in bytes (0: none).
fn file_and_wal_bytes(path: &std::path::Path) -> u64 {
    let wal = std::path::PathBuf::from(format!("{}-wal", path.display()));
    [path, wal.as_path()].iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum()
}

/// What `kobd db info` reports about the data directory of `cfg`: read-only and offline (no writer lock, no node, no schema
/// change: a store of an older schema is described, not migrated).
pub fn store_info(cfg: &IndexerConfig) -> Result<serde_json::Value> {
    use crate::indexer::db::{meta_get, SCHEMA_VERSION};
    use serde_json::json;
    let path = std::path::absolute(cfg.db_path()).unwrap_or_else(|_| cfg.db_path());
    let data_dir = std::path::absolute(&cfg.data_dir).unwrap_or_else(|_| cfg.data_dir.clone());
    let mut v = json!({
        "data_dir": data_dir.display().to_string(),
        "database": path.display().to_string(),
        "exists": path.exists(),
        "network_configured": cfg.network,
        "lean_configured": cfg.lean,
        "build_schema": SCHEMA_VERSION,
    });
    if cfg.records.enabled && !cfg.lean {
        v["record_log_bytes"] = json!(crate::indexer::recordlog::log_bytes(&cfg.records_dir()).unwrap_or(0));
    }
    if !path.exists() {
        return Ok(v);
    }
    let conn = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("cannot open {} read-only", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let meta = |k: &str| meta_get(&conn, k).ok().flatten();
    let schema: i64 = meta("schema_version").and_then(|s| s.parse().ok()).unwrap_or(0);
    v["schema"] = json!(schema);
    v["schema_state"] = json!(match schema {
        s if s == SCHEMA_VERSION => "current".to_string(),
        s if s > SCHEMA_VERSION => format!("newer than this build reads ({SCHEMA_VERSION}): run the newer build"),
        s if s >= crate::indexer::storage::MIGRATE_FROM_SCHEMA => "older: `kobd db migrate` brings it to this build's".to_string(),
        _ => "older than `kobd db migrate` takes: rebuild it with `kobd db replay`".to_string(),
    });
    v["network"] = json!(meta("network"));
    v["cursor_daa"] = json!(meta("cursor_daa").and_then(|s| s.parse::<u64>().ok()));
    v["cursor_hash"] = json!(meta("cursor_hash"));
    v["history_from_daa"] = json!(meta("history_from_daa").and_then(|s| s.parse::<u64>().ok()));
    v["orders"] = json!(conn.query_row("SELECT COUNT(*) FROM orders", [], |r| r.get::<_, i64>(0)).ok());
    // a lean store gives freed pages back to the file (`auto_vacuum = INCREMENTAL`, `indexer::lean`)
    let vacuum: i64 = conn.query_row("PRAGMA auto_vacuum", [], |r| r.get(0)).unwrap_or(0);
    v["storage"] = json!(if vacuum == 2 { "lean" } else { "archive" });
    v["database_bytes"] = json!(file_and_wal_bytes(&path));
    Ok(v)
}

/// `kobd db info`.
fn db_info(opts: &IndexOpts, json: bool) -> Result<()> {
    let cfg = build_config(opts)?;
    let v = store_info(&cfg)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    for line in store_info_lines(&v) {
        println!("{line}");
    }
    Ok(())
}

/// [`store_info`] as lines for people.
pub fn store_info_lines(v: &serde_json::Value) -> Vec<String> {
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    let n = |k: &str| v[k].as_u64().map(|x| x.to_string()).unwrap_or_else(|| "unknown".into());
    let mb = |k: &str| v[k].as_u64().map(|b| format!("{:.1} MB", b as f64 / 1_048_576.0)).unwrap_or_else(|| "-".into());
    let mut out = vec![format!("Data directory: {}", s("data_dir"))];
    if !v["exists"].as_bool().unwrap_or(false) {
        out.push(format!("Database: {} does not exist yet (the first start creates it)", s("database")));
        return out;
    }
    out.push(format!("Database: {} ({})", s("database"), mb("database_bytes")));
    out.push(format!("Schema: {} ({}; this build reads {})", n("schema"), s("schema_state"), n("build_schema")));
    let net = s("network");
    let configured = s("network_configured");
    out.push(if net == configured || net.is_empty() {
        format!("Network: {}", if net.is_empty() { "not recorded".to_string() } else { net })
    } else {
        format!("Network: {net} (the configuration says {configured}: this store is another network's)")
    });
    out.push(format!(
        "Storage: {} (the configuration says {})",
        s("storage"),
        if v["lean_configured"] == true { "lean" } else { "archive" }
    ));
    out.push(format!("Cursor: DAA {} (block {})", n("cursor_daa"), v["cursor_hash"].as_str().unwrap_or("none yet")));
    out.push(format!("Orders: {}", n("orders")));
    out.push(match v["history_from_daa"].as_u64() {
        Some(d) => format!("History: from DAA {d} (older orders are not in this store: book_complete false)"),
        None => "History: complete from the start point the node retained".to_string(),
    });
    if v.get("record_log_bytes").is_some() {
        out.push(format!("Record log: {}", mb("record_log_bytes")));
    }
    out
}

fn ms_text(ms: i64) -> String {
    // seconds since the epoch are enough for an operator who pipes it to `date -d @`; keep the table dependency-free
    format!("{}", ms / 1000)
}

/// `kobd logo-reports`: the administrator's side of the blocklist (`crate::logo_reports`).
pub fn run_logo_reports(a: LogoReportsArgs) -> Result<()> {
    use crate::hex::Hash32;
    use crate::logo_reports as lr;
    let (cfg, cmd) = (build_config(&IndexOpts::of_store(&a.store, None))?, a.cmd);
    let mut conn = lr::open(&cfg.db_path(), &cfg.network)?;
    let now = lr::system_now_ms();
    let id = |s: &str| Hash32::parse(s.trim()).map_err(|e| anyhow!("covenant id: {e}"));
    match cmd {
        LogoCmd::List { json } => {
            let rows = lr::list(&conn)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else if rows.is_empty() {
                println!("no token has been reported");
            } else {
                println!(
                    "{:<64}  {:<9} {:>7}  {:<14} {:>11} {:>11}  issues",
                    "covenant id", "state", "reports", "ill/imp/oth", "hidden at", "restored at"
                );
                for r in rows {
                    let by = format!("{}/{}/{}", r.by_reason[0], r.by_reason[1], r.by_reason[2]);
                    let issues = r.issues.iter().map(|n| format!("#{n}")).collect::<Vec<_>>().join(",");
                    println!(
                        "{:<64}  {:<9} {:>7}  {:<14} {:>11} {:>11}  {}",
                        r.covenant_id,
                        r.state,
                        r.reports,
                        by,
                        ms_text(r.hidden_ms),
                        r.restored_ms.map_or("-".into(), ms_text),
                        issues
                    );
                }
                println!("times are Unix seconds. hidden = hidden by a report, not yet reviewed; restore it, or hide it for good.");
            }
        }
        LogoCmd::Restore { covenant_id, force } => {
            lr::restore(&mut conn, &id(&covenant_id)?, now, force)?;
            println!(
                "restored {}: its logo shows again; reports in the next {} s are only recorded",
                covenant_id.trim(),
                cfg.logo_reports.cooldown_secs
            );
        }
        LogoCmd::Hide { covenant_id } => {
            lr::hide_for_good(&mut conn, &id(&covenant_id)?, now)?;
            println!("hidden for good: {}", covenant_id.trim());
        }
        LogoCmd::Export => println!("{}", serde_json::to_string_pretty(&lr::export(&conn)?)?),
        LogoCmd::Import { file } => {
            let doc: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&file).with_context(|| file.display().to_string())?)?;
            let (tokens, rows) = lr::import(&mut conn, &doc).map_err(|e| anyhow!(e))?;
            println!("imported {tokens} listed token(s) and {rows} report(s)");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        opts: IndexOpts,
    }

    fn cfg(args: &[&str]) -> IndexerConfig {
        // the build's own network (a deployment build refuses another one)
        let mut v = vec!["kobd", "--network", kob_protocol::artifacts::deployment_network().unwrap_or("testnet-10")];
        v.extend_from_slice(args);
        build_config(&T::parse_from(v).opts).unwrap()
    }

    /// The KAS/USD price route is opt-in: off unless `--usd-price` (or `usd_price` in `[api]`, or `KOB_USD_PRICE`) says so.
    #[test]
    fn the_usd_price_route_is_off_by_default() {
        let _g = crate::rpc::resolver::tests::serial();
        assert!(!cfg(&[]).api.usd_price);
        assert!(cfg(&["--usd-price"]).api.usd_price);
        let c: IndexerConfig = toml::from_str(
            "[api]
usd_price = true",
        )
        .unwrap();
        assert!(c.api.usd_price);
    }

    /// `resolver` is always a pair of public nodes; `--extra-node resolver` adds a third; an own node with `--extra-node
    /// resolver` is a primary and one public node. The option of earlier builds, `--node`, is refused with the new name.
    #[test]
    fn resolver_entries_come_as_a_pair_and_number_themselves() {
        // build_config sets the resolver state's network: not while the resolver tests use it
        let _g = crate::rpc::resolver::tests::serial();
        let urls = |c: &IndexerConfig| c.nodes.iter().map(|n| n.url.clone()).collect::<Vec<_>>();
        let c = cfg(&["--rpc-url", "resolver"]);
        assert_eq!((c.rpc_url.as_str(), urls(&c)), ("resolver", vec!["resolver:2".to_string()]));
        assert_eq!(c.secondaries().len(), 1);
        let c = cfg(&["--rpc-url", "resolver", "--extra-node", "resolver"]);
        assert_eq!(urls(&c), ["resolver:2", "resolver:3"]);
        let c = cfg(&["--rpc-url", "ws://127.0.0.1:18210", "--extra-node", "resolver"]);
        assert_eq!(urls(&c), ["resolver:2"]);
        // `--node` never silently becomes a further node (nor the node): a usage error that names both options
        let net = kob_protocol::artifacts::deployment_network().unwrap_or("testnet-10");
        let e = build_config(&T::parse_from(["kobd", "--network", net, "--node", "ws://127.0.0.1:18210"]).opts).unwrap_err();
        assert!(e.downcast_ref::<crate::cli::Usage>().is_some() && e.to_string() == OLD_NODE_OPTION, "{e}");
        // no node named: the public pair by default
        let c = cfg(&[]);
        assert_eq!((c.rpc_url.as_str(), urls(&c)), ("resolver", vec!["resolver:2".to_string()]));
        // a configured node takes precedence
        let c = cfg(&["--rpc-url", "ws://127.0.0.1:18210"]);
        assert!(c.nodes.is_empty() && c.rpc_url == "ws://127.0.0.1:18210");
    }

    /// `[failover]` is for a configured node and its only fallback is the public pair: anything else is refused, not ignored.
    #[test]
    fn the_failover_falls_back_to_the_public_pair_from_a_configured_node_only() {
        let _g = crate::rpc::resolver::tests::serial();
        let parse = |toml: &str| {
            let mut c = toml::from_str::<IndexerConfig>(toml).unwrap();
            number_resolver_entries(&mut c);
            c
        };
        let own = "rpc_url = \"ws://10.0.0.5:18210\"\n";
        assert!(check_failover(&parse(own)).is_ok(), "off by default");
        let on = parse(&format!("{own}[failover]\nfallback = \"resolver\"\n"));
        assert!(check_failover(&on).is_ok());
        assert_eq!((on.failover.after_failures, on.failover.after_secs, on.failover.probe_secs), (5, 60, 30));
        let e = check_failover(&parse(&format!("{own}[failover]\nfallback = \"wss://other.example\"\n"))).unwrap_err();
        assert!(e.to_string().contains("the only fallback is \"resolver\""), "{e}");
        for pub_cfg in ["rpc_url = \"resolver\"\n", "rpc_url = \"ws://10.0.0.5:18210\"\n[[nodes]]\nurl = \"resolver\"\n"] {
            let e = check_failover(&parse(&format!("{pub_cfg}[failover]\nfallback = \"resolver\"\n"))).unwrap_err();
            assert!(e.to_string().contains("uses the public nodes already"), "{e}");
        }
        // the fallback client is the public pair
        let f = fallback_client(&on);
        assert_eq!(
            f.specs().iter().map(|s| (s.url.as_str(), s.cross_check)).collect::<Vec<_>>(),
            [("resolver", false), ("resolver:2", true)]
        );
        // the Executor refuses it
        let e = crate::executor::refuse_failover(&on).unwrap_err();
        assert!(e.to_string().contains("an option of the Indexer"), "{e}");
        assert!(crate::executor::refuse_failover(&parse(own)).is_ok());
    }

    /// `--config <dir>/kobd.toml` from another directory reads the files next to the configuration (the setup's
    /// relative `data_dir`, `tokens_path`, `[records] dir`); absolute paths, a bare file name (the launcher, run in the
    /// file's directory) and paths given as flags are left as they are.
    #[test]
    fn relative_paths_of_a_config_file_are_taken_from_its_directory() {
        let _g = crate::rpc::resolver::tests::serial();
        let d = tempfile::tempdir().unwrap();
        let abs = d.path().join("elsewhere").join("tokens.json");
        let file = d.path().join("kobd.toml");
        let toml = |tokens: &std::path::Path| {
            format!("data_dir = \"kob-data\"\ntokens_path = {:?}\n[records]\ndir = \"logs/records\"\n", tokens.to_string_lossy())
        };
        std::fs::write(&file, toml(std::path::Path::new("kob-tokens.json"))).unwrap();
        let path = file.to_string_lossy().into_owned();
        let c = cfg(&["--config", &path]);
        assert_eq!(c.data_dir, d.path().join("kob-data"));
        assert_eq!(c.tokens_path, Some(d.path().join("kob-tokens.json")));
        assert_eq!(c.records_dir(), d.path().join("logs/records"));
        // flags win and stay relative to the working directory
        let c = cfg(&["--config", &path, "--data-dir", "here", "--tokens", "t.json"]);
        assert_eq!((c.data_dir, c.tokens_path), (PathBuf::from("here"), Some(PathBuf::from("t.json"))));
        // an absolute path in the file is kept
        std::fs::write(&file, toml(&abs)).unwrap();
        assert_eq!(cfg(&["--config", &path]).tokens_path, Some(abs));
        // a bare file name: the paths as written (the launcher changes to the file's directory first)
        let mut c: IndexerConfig = toml::from_str("data_dir = \"kob-data\"\ntokens_path = \"kob-tokens.json\"").unwrap();
        relative_to_config(&mut c, std::path::Path::new("kobd.toml"));
        assert_eq!((c.data_dir, c.tokens_path), (PathBuf::from("kob-data"), Some(PathBuf::from("kob-tokens.json"))));
    }

    /// The Indexer (`kobd --indexer`) refuses what it would otherwise ignore: the options of role `run` its configuration
    /// sets (the launcher refuses the same `[launch]` keys), and `fast_start`, which needs the matcher the index role does not have.
    #[test]
    fn index_refuses_the_options_of_role_run_instead_of_ignoring_them() {
        // build_config sets the resolver state's network: not while the resolver tests use it
        let _g = crate::rpc::resolver::tests::serial();
        let parse = |toml: &str| toml::from_str::<IndexerConfig>(toml).unwrap();
        let file = std::path::Path::new("kobd.toml");
        // a keyless index configuration is fine, with or without a [launch] table
        assert!(refuse_run_only_options(&parse(""), Some(file)).is_ok());
        let keyless = "[launch]\nrole = \"index\"\nargs = [\"--status-secs\", \"30\"]";
        assert!(refuse_run_only_options(&parse(keyless), Some(file)).is_ok());
        assert!(refuse_run_only_options(&parse("start = \"sink\"\nfast_start = false"), None).is_ok());
        for key in ["key_file", "pause_file", "x402_config"] {
            let c = parse(&format!("[launch]\nrole = \"index\"\n{key} = \"some-file\""));
            let e = refuse_run_only_options(&c, Some(file)).unwrap_err().to_string();
            assert!(e.starts_with("kobd.toml: ") && e.contains(&format!("[launch] {key} is an option of role \"run\"")), "{e}");
        }
        // fast start: refused with the way out (the sink start) named
        let e = refuse_run_only_options(&parse("fast_start = true"), Some(file)).unwrap_err().to_string();
        assert!(
            e.contains("fast_start is an option of the Executor (`kobd --executor`")
                && e.contains("start = \"sink\"")
                && e.contains("book_complete"),
            "{e}"
        );
        // and the flag does the same: --fast-start switches it on for one start
        let c = cfg(&["--fast-start"]);
        assert!(c.fast_start && refuse_run_only_options(&c, None).is_err());
        assert!(refuse_run_only_options(&cfg(&["--start", "sink"]), None).is_ok());
    }
}
