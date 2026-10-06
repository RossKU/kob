//! `kob-executor`: one binary with role subcommands.
//!
//! * `run`: the indexer, the matcher and the keepers in one process over one store, plus the x402
//!   facilitator with `--x402-config` (the operator's default; docs/ops/executor.md);
//! * `index`: the indexer and its read API alone (the public, keyless role);
//! * `match`, `keep`: the matcher and the keepers alone, planning against a book snapshot file that
//!   an indexer exports (`--book-file`), for deployments that keep the key away from the public API;
//! * `x402`: the x402 facilitator alone (keyless; finality from the node's UTXO set).
//!
//! Roles that hold keys hold only the operator's hot key; the facilitator holds none.

use clap::{Parser, Subcommand};
use kob_executor::executor::{self, RunArgs};
use kob_executor::index_cli::{run_index, IndexArgs};
use kob_executor::matcher::cli::{run_keep, run_match, KeepArgs, MatchArgs};
use kob_executor::x402::{self, X402Args};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "kob-executor", version = kob_protocol::VERSION, about = "KOB executor: indexer, matcher, keepers and x402 facilitator")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Indexer + matcher + keepers (+ x402 facilitator) in one process, sharing one store (needs the operator key).
    Run(Box<RunArgs>),
    /// Follow the node, index KOB orders and serve the read API (REST + WebSocket).
    Index(IndexArgs),
    /// Batch-match crossing orders (spread + tips), trigger and arm stops in the batches, chain oversize crossings, from a
    /// book snapshot file.
    Match(MatchArgs),
    /// Refund / kill / close expired orders (paid by the orders' tips), sweep own strays, from a book snapshot file.
    Keep(KeepArgs),
    /// The x402 facilitator alone: GET /supported, POST /verify, POST /settle, GET /health, GET /metrics.
    X402(X402Args),
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))).init();
    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async {
        match cli.cmd {
            Cmd::Run(a) => executor::run(*a).await,
            Cmd::Index(a) => run_index(a).await,
            Cmd::Match(a) => run_match(a).await,
            Cmd::Keep(a) => run_keep(a).await,
            Cmd::X402(a) => x402::run(a).await,
        }
    })
}
