//! The indexer's command line: the options of a start (`kobd --indexer`, and the set `kobd --executor` shares), the
//! database tools, `kobd db <tool>` (migrate, replay, export-orders, export-book, import-orders, rebase), and the token-logo
//! reports, `kobd logo-reports <command>`.

use crate::config::NodeConfig;
use crate::config::{IndexerConfig, StartMode};
use crate::indexer::{self, Indexer};
use crate::recover::{self, MakerExport};
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
    pub opts: IndexOpts,
    #[command(subcommand)]
    pub sub: DbSub,
}

/// `kobd logo-reports <command>`.
#[derive(Args)]
pub struct LogoReportsArgs {
    #[command(flatten)]
    pub opts: IndexOpts,
    #[command(subcommand)]
    pub cmd: LogoCmd,
}

/// The indexer options every start takes (`kobd --indexer`, `kobd --executor`), and the tools of `kobd db` and
/// `kobd logo-reports`.
#[derive(Args, Clone, Debug)]
pub struct IndexOpts {
    /// TOML configuration file (all keys optional); flags override it.
    #[arg(long, global = true, env = "KOB_INDEX_CONFIG")]
    pub config: Option<PathBuf>,
    /// Network id the node must report (`mainnet`, `testnet-10`). Required on the reference build (no default); a deployment
    /// build (`deploy-mainnet`, `deploy-tn10`) implies its own and refuses another.
    #[arg(long, global = true, env = "KOB_NETWORK")]
    pub network: Option<String>,
    /// Node wRPC endpoint: JSON wRPC (`ws://`, `wss://`), Borsh wRPC (a `.../borsh` path or a default Borsh port `17110` /
    /// `17210`: every call in Borsh), or `resolver`: a public Kaspa node of the network found through the Kaspa resolvers
    /// (Borsh over TLS; another one after a failure). Default: the local node's JSON port of the network
    /// (`ws://127.0.0.1:18110` on mainnet, `ws://127.0.0.1:18210` on testnet-10). A URL with a password or an API key belongs
    /// in the config file (`rpc_url`) or in `KOB_NODE_WRPC`, not on the command line (`ps`).
    #[arg(long, global = true, env = "KOB_NODE_WRPC")]
    pub rpc_url: Option<String>,
    /// Another node (`ws://` or `wss://`, JSON or Borsh wRPC as `--rpc-url`, or `resolver`: another public node, resolved
    /// independently of the primary's; repeatable): windows of transaction bodies are fetched from it too (checked against
    /// the primary, `--rpc-url`) and transactions submitted to it too. Added to the config's `nodes`.
    /// Command-line arguments are visible to other local users (`ps`): a URL with a password or an API key belongs in the
    /// config file (`[[nodes]]`) or in `KOB_INDEX_NODES` (comma-separated).
    #[arg(long = "node", global = true, value_name = "URL", env = "KOB_INDEX_NODES", value_delimiter = ',')]
    pub nodes: Vec<String>,
    /// With `--node`: the primary serves windows of transaction bodies only when no other node can (it stays the chain
    /// authority).
    #[arg(long, global = true)]
    pub no_primary_fetch: bool,
    /// Fetch the windows of transaction bodies over each node's Borsh wRPC endpoint (`borsh` in the config).
    #[arg(long, global = true)]
    pub borsh: bool,
    /// Parallel windows while the cursor is more than this many blue score behind the sink (`prefetch_min_lag_blue`).
    #[arg(long, global = true)]
    pub prefetch_min_lag_blue: Option<u64>,
    /// Directory holding the SQLite database and the record log.
    #[arg(long, global = true, env = "KOB_INDEX_DATA_DIR")]
    pub data_dir: Option<PathBuf>,
    /// Token allowlist JSON (`registry/tokens.json`).
    #[arg(long, global = true)]
    pub tokens: Option<PathBuf>,
    /// Where a fresh database starts: `pruning-point` (default), `sink`, or a chain block hash.
    #[arg(long, global = true)]
    pub start: Option<StartMode>,
    /// The Executor on an empty data directory: trade at once on a store that follows the chain from the node's sink, and index the
    /// history into the normal store meanwhile (`fast_start` in the configuration; docs/ops/executor.md, *Fast start*).
    #[arg(long, global = true, conflicts_with = "no_fast_start")]
    pub fast_start: bool,
    /// Turn `fast_start` of the configuration off for this start.
    #[arg(long, global = true)]
    pub no_fast_start: bool,
    /// Lean storage: keep the live book and 30 days of closed orders (500 MB cap), no record log, a file that shrinks (`lean` in the
    /// configuration; docs/ops/executor.md, *Lean storage*).
    #[arg(long, global = true, conflicts_with = "no_lean")]
    pub lean: bool,
    /// Turn `lean` of the configuration off for this start.
    #[arg(long, global = true)]
    pub no_lean: bool,
    /// Lean storage: days closed orders stay readable (`lean_retention_days`, default 30).
    #[arg(long, global = true)]
    pub lean_retention_days: Option<u64>,
    /// Lean storage: size cap of the database in MB (`lean_max_db_mb`, default 500; 0: none).
    #[arg(long, global = true)]
    pub lean_max_db_mb: Option<u64>,
    /// API listen address, e.g. `127.0.0.1:8090`.
    #[arg(long, global = true)]
    pub listen: Option<std::net::SocketAddr>,
    /// Do not serve the read API.
    #[arg(long, global = true)]
    pub no_api: bool,
    /// Disable the permanent record log (not recommended: it is the rebuild source, see docs/ops/executor.md, Part B).
    #[arg(long, global = true)]
    pub no_record_log: bool,
    /// Hours of chain blocks kept for reorgs (the finality window; default 12).
    #[arg(long, global = true)]
    pub reorg_window_hours: Option<u64>,
    /// Log a one-line status (the indexer's lag; with the Executor also the transactions sent, the profit, the funds and the
    /// warnings) every this many seconds, as kaspad logs one every 10 s; 0: never.
    #[arg(long, global = true, default_value_t = 10)]
    pub status_secs: u64,
}

impl IndexOpts {
    /// `--status-secs` as a period (`None`: off).
    pub fn status_every(&self) -> Option<std::time::Duration> {
        (self.status_secs > 0).then(|| std::time::Duration::from_secs(self.status_secs))
    }
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
    /// Bring a data directory written by an earlier build (schema 5, 6, 7 or 8) to the compact layout (schema 9) in one run: a new
    /// store is built beside the old one (integer ids, packed state spans, fills-only indexes), verified row by row against it
    /// and swapped in, and the sealed record-log segments are gzip-compressed. Stop the executor first (it takes the writer
    /// lock); this build refuses an older store until this has run. Safe to repeat or to stop (the old store is not touched
    /// before the swap); the previous store file is kept beside the new one (`index.sqlite3.before-migrate`) until you delete it.
    /// Needs free disk for the new store (about three quarters of the old one) and the temporary files of the index builds.
    Migrate {
        /// Do not compress the sealed record-log segments.
        #[arg(long)]
        no_compress_log: bool,
    },
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
    ImportOrders { file: PathBuf },
    /// After a gap: move the cursor to a fresh start point and reconcile the tracked orders with the node.
    Rebase {
        /// `sink` or a chain block hash.
        #[arg(long, default_value = "sink")]
        to: StartMode,
        /// Only move the cursor; do not touch the tracked orders (they may stay open although spent).
        #[arg(long)]
        no_reconcile: bool,
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
    let mut cfg = match &a.config {
        Some(p) => {
            let mut c = IndexerConfig::load(p)?;
            relative_to_config(&mut c, p);
            c
        }
        None => IndexerConfig::default(),
    };
    if let Some(v) = &a.network {
        cfg.network = v.clone();
    }
    if let Some(v) = &a.rpc_url {
        cfg.rpc_url = v.clone();
    }
    if a.no_primary_fetch {
        cfg.primary_fetch = false;
    }
    if let Some(v) = a.prefetch_min_lag_blue {
        cfg.prefetch_min_lag_blue = v;
    }
    if a.borsh {
        cfg.borsh = true;
    }
    for url in &a.nodes {
        if !cfg.nodes.iter().any(|n| &n.url == url) {
            cfg.nodes.push(NodeConfig { url: url.clone(), ..NodeConfig::default() });
        }
    }
    if let Some(v) = &a.data_dir {
        cfg.data_dir = v.clone();
    }
    if let Some(v) = &a.tokens {
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
        cfg.rpc_url = crate::config::default_wrpc_url(&cfg.network).ok_or_else(|| {
            anyhow!("no node endpoint for network {}: pass --rpc-url (or `rpc_url` in the configuration)", cfg.network)
        })?;
    }
    number_resolver_entries(&mut cfg);
    // the resolver entries resolve public nodes of this network
    crate::rpc::resolver::set_network(&cfg.network);
    Ok(cfg)
}

/// Gives every resolver entry of the configuration its own pseudo URL (`rpc::resolver::pseudo`), so each resolves a public
/// node of its own on another host: the primary's `resolver` stays `resolver`; a primary `resolver` always comes as a pair, the
/// second entry `resolver:2` (added first, before any other node), which fetches windows, takes every submission and vouches
/// for the primary's answers (`rpc::multi`, `NodeSpec::cross_check`); every other node `resolver` (`--node resolver`) becomes
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
    refuse_run_only_options(&cfg, opts.config.as_deref())?;
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
    let cfg = build_config(&a.opts)?;
    match a.sub {
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
        DbSub::ImportOrders { file } => {
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
        DbSub::Rebase { to, no_reconcile } => {
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

fn ms_text(ms: i64) -> String {
    // seconds since the epoch are enough for an operator who pipes it to `date -d @`; keep the table dependency-free
    format!("{}", ms / 1000)
}

/// `kobd logo-reports`: the administrator's side of the blocklist (`crate::logo_reports`).
pub fn run_logo_reports(a: LogoReportsArgs) -> Result<()> {
    use crate::hex::Hash32;
    use crate::logo_reports as lr;
    let (cfg, cmd) = (build_config(&a.opts)?, a.cmd);
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

    /// `resolver` is always a pair of public nodes; `--node resolver` adds a third; an own node with `--node resolver` is a
    /// primary and one public node.
    #[test]
    fn resolver_entries_come_as_a_pair_and_number_themselves() {
        // build_config sets the resolver state's network: not while the resolver tests use it
        let _g = crate::rpc::resolver::tests::serial();
        let urls = |c: &IndexerConfig| c.nodes.iter().map(|n| n.url.clone()).collect::<Vec<_>>();
        let c = cfg(&["--rpc-url", "resolver"]);
        assert_eq!((c.rpc_url.as_str(), urls(&c)), ("resolver", vec!["resolver:2".to_string()]));
        assert_eq!(c.secondaries().len(), 1);
        let c = cfg(&["--rpc-url", "resolver", "--node", "resolver"]);
        assert_eq!(urls(&c), ["resolver:2", "resolver:3"]);
        let c = cfg(&["--rpc-url", "ws://127.0.0.1:18210", "--node", "resolver"]);
        assert_eq!(urls(&c), ["resolver:2"]);
        let c = cfg(&[]);
        let own = crate::config::default_wrpc_url(&c.network).unwrap();
        assert!(c.nodes.is_empty() && c.rpc_url == own, "an own node by default on the command line");
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
