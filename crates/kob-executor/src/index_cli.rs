//! The indexer's command line: `kob-executor index` (and the option set `kob-executor run` shares).

use crate::config::NodeConfig;
use crate::config::{IndexerConfig, StartMode};
use crate::hex::Hash32;
use crate::indexer::ingest::Cursor;
use crate::indexer::{self, Indexer};
use crate::recover::{self, MakerExport};
use crate::rpc::multi::{MultiNode, NodeSpec};
use crate::rpc::{ChainSource, WrpcClient, WrpcConfig};
use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;

/// `kob-executor index`.
#[derive(Args)]
pub struct IndexArgs {
    #[command(flatten)]
    pub opts: IndexOpts,
    #[command(subcommand)]
    pub sub: Option<IndexSub>,
}

/// The indexer options every role that embeds the indexer takes (`index`, `run`).
#[derive(Args, Clone, Debug)]
pub struct IndexOpts {
    /// TOML configuration file (all keys optional); flags override it.
    #[arg(long, global = true, env = "KOB_INDEX_CONFIG")]
    pub config: Option<PathBuf>,
    /// Network id the node must report (`testnet-10`, `mainnet`).
    #[arg(long, global = true)]
    pub network: Option<String>,
    /// Node JSON wRPC endpoint (`ws://127.0.0.1:18210`).
    #[arg(long, global = true, env = "KOB_NODE_WRPC")]
    pub rpc_url: Option<String>,
    /// Another node (`ws://` or `wss://` JSON wRPC; repeatable): windows of transaction bodies are fetched from it too
    /// (checked against the primary, `--rpc-url`) and transactions submitted to it too. Added to the config's `nodes`.
    #[arg(long = "node", global = true, value_name = "URL")]
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
}

#[derive(Subcommand)]
pub enum IndexSub {
    /// Follow the node and serve the API (the default when no subcommand is given).
    Run,
    /// Rebuild a NEW database from the record log (the log is not modified), then follow the node from its cursor.
    Replay,
    /// Print a maker order export (JSON) from the database.
    ExportOrders {
        /// Maker public key (hex); default: all makers.
        #[arg(long)]
        maker: Option<String>,
        /// Only open and partially filled orders.
        #[arg(long)]
        live_only: bool,
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

pub fn build_config(a: &IndexOpts) -> Result<IndexerConfig> {
    let mut cfg = match &a.config {
        Some(p) => IndexerConfig::load(p)?,
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
    if a.no_api {
        cfg.api.enabled = false;
    }
    if a.no_record_log {
        cfg.records.enabled = false;
    }
    if let Some(v) = a.reorg_window_hours {
        cfg.reorg_window_hours = v;
    }
    // A deployment build (e.g. `--features deploy-tn10`) records the network it was built for (protocol v2.6: the templates
    // are the reference ones; the build only refuses to index another network).
    if let Some(n) = kob_protocol::artifacts::deployment_network() {
        if cfg.network != n {
            return Err(anyhow!("this binary is the {n} deployment build but the configured network is {}", cfg.network));
        }
    }
    Ok(cfg)
}

fn wrpc(cfg: &IndexerConfig, url: &str, vspc_connections: usize) -> Arc<WrpcClient> {
    let mut w = WrpcConfig::new(url.to_string());
    w.request_timeout = std::time::Duration::from_secs(cfg.rpc_timeout_secs);
    w.vspc_connections = vspc_connections;
    // Borsh only on VSPC connections of its own (with none, VSPC shares the JSON connection)
    w.borsh_url = cfg.borsh_url_of(url).filter(|_| vspc_connections > 0);
    if let Some(b) = &w.borsh_url {
        tracing::info!(node = url, borsh = %b, connections = vspc_connections, "windows of transaction bodies over Borsh wRPC");
    }
    if cfg.borsh && w.borsh_url.is_none() && vspc_connections > 0 {
        tracing::warn!(node = url, "no Borsh endpoint known for this node: its windows stay JSON (set borsh_url)");
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
    let primary_spec = NodeSpec { url: primary_url.clone(), fetch: cfg.primary_fetches(), submit: true, connections: parallel };
    let mut nodes = vec![(primary_spec, primary.clone())];
    for n in cfg.secondaries() {
        let c = n.connections.unwrap_or(parallel).max(1);
        let spec = NodeSpec { url: n.url.clone(), fetch: n.fetch && parallel > 1, submit: n.submit, connections: c };
        nodes.push((spec, wrpc(cfg, &n.url, conns(c))));
    }
    // the acceptance data and the primary's copies of chain blocks that matter, on connections of their own
    let verifier = if nodes.len() > 1 { wrpc(cfg, &primary_url, 4) } else { primary };
    if nodes.len() > 1 {
        tracing::info!(
            primary = %primary_url,
            others = ?nodes[1..].iter().map(|(s, _)| s.url.as_str()).collect::<Vec<_>>(),
            windows = cfg.fetch_windows(),
            "several nodes: bodies from all (checked against the primary), the chain from the primary"
        );
    }
    Arc::new(MultiNode::new(nodes, verifier))
}

pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// Runs the indexer command.
pub async fn run_index(a: IndexArgs) -> Result<()> {
    let cfg = build_config(&a.opts)?;
    match a.sub.unwrap_or(IndexSub::Run) {
        IndexSub::Run => {
            tracing::info!(network = %cfg.network, rpc = %cfg.rpc_url, data = %cfg.data_dir.display(), "starting indexer");
            let idx = Indexer::open(cfg)?;
            idx.run(shutdown_signal()).await?;
        }
        IndexSub::Replay => {
            let r = indexer::replay_from_log(&cfg)?;
            println!(
                "replayed {} record-log frames (format 1: {}, format 2: {}; {} relevant transactions) into {}: {} orders",
                r.frames,
                r.formats[1],
                r.formats[2],
                r.relevant,
                cfg.db_path().display(),
                r.orders
            );
            if !r.skipped.is_empty() || r.dropped.dropped() > 0 {
                println!(
                    "skipped {} frame(s) this build cannot decode {:?}; dropped {} retired and {} unknown reveal(s), {} holding(s), {} import(s) of template layouts this build does not have",
                    r.skipped.len(),
                    r.skipped.iter().map(|(n, _)| n).collect::<Vec<_>>(),
                    r.dropped.retired_reveals,
                    r.dropped.unknown_reveals,
                    r.dropped.unknown_holds,
                    r.dropped.unknown_imports
                );
            }
        }
        IndexSub::ExportOrders { maker, live_only } => {
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
        IndexSub::ImportOrders { file } => {
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
        IndexSub::Rebase { to, no_reconcile } => {
            let idx = Indexer::open(cfg.clone())?;
            let client = node_client(&cfg);
            let info = client.server_info().await?;
            if info.network_id != cfg.network || !info.is_synced {
                return Err(anyhow!("node reports network `{}` synced={}", info.network_id, info.is_synced));
            }
            let dag = client.dag_info().await?;
            let hash: Hash32 = match to {
                StartMode::Sink => dag.sink,
                StartMode::PruningPoint => dag.pruning_point_hash,
                StartMode::Hash(h) => h,
            };
            let cursor = Cursor { hash, daa: dag.virtual_daa_score };
            if no_reconcile {
                let mut g = idx.ingest.lock().unwrap_or_else(|e| e.into_inner());
                g.apply_ops(vec![], Some(cursor))?;
                println!("cursor moved to {hash}; tracked orders were NOT reconciled");
            } else {
                let rep = recover::reconcile_open_orders(&idx.ingest, client.as_ref(), &cfg.network, cursor).await?;
                println!("cursor moved to {hash}; closed {} spent outputs, adopted {} continuations", rep.closed, rep.adopted);
            }
        }
    }
    Ok(())
}
