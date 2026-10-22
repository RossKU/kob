//! `kob-executor`: one binary with role subcommands.
//!
//! * `run`: the indexer, the matcher and the keepers in one process over one store, plus the x402
//!   facilitator with `--x402-config` (the operator's default; docs/ops/executor.md);
//! * `index`: the indexer and its read API alone (the public, keyless role);
//! * `match`, `keep`: the matcher and the keepers alone, planning against a book snapshot file that
//!   an indexer exports (`--book-file`), for deployments that keep the key away from the public API;
//! * `x402`: the x402 facilitator alone (keyless; finality from the node's UTXO set);
//! * `setup`: the interactive first run (`kob_executor::setup`), which writes `kob-executor.toml` next to the binary.
//!
//! Without arguments (a double click) the binary runs `kob-executor.toml` from its own directory, or the setup when there is
//! none (docs/ops/executor-quickstart.md).
//!
//! Roles that hold keys hold only the operator's hot key; the facilitator holds none.

use std::ffi::OsString;
use std::io::IsTerminal;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use kob_executor::config::LaunchRole;
use kob_executor::executor::{self, RunArgs};
use kob_executor::index_cli::{run_index, IndexArgs};
use kob_executor::matcher::cli::{run_keep, run_match, KeepArgs, MatchArgs};
use kob_executor::setup::{self, NoArgs, SetupArgs, SetupOptions, StdConsole, WrpcSetupNode};
use kob_executor::x402::{self, X402Args};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "kob-executor",
    version = kob_protocol::VERSION,
    about = "KOB executor: indexer, matcher, keepers and x402 facilitator",
    after_help = "Without arguments: runs kob-executor.toml from the executable's directory, or the setup when there is none."
)]
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
    /// First run: check the node, make the operator key, wait for funds, write kob-executor.toml next to the binary, start.
    Setup(SetupArgs),
}

fn main() -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().collect();
    // a double click: no arguments, a console a person reads
    let double_click = argv.len() == 1 && std::io::stdin().is_terminal();
    match start(argv) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e:?}");
            if double_click {
                // the console window closes with the process: keep the error on screen until it is read
                eprintln!();
                eprintln!("Press Enter to close this window.");
                let _ = setup::read_line();
            }
            ExitCode::FAILURE
        }
    }
}

fn start(argv: Vec<OsString>) -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))).init();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let program = argv.first().cloned().unwrap_or_else(|| "kob-executor".into());
    if argv.len() > 1 {
        let cli = Cli::parse_from(argv);
        if let Cmd::Setup(a) = &cli.cmd {
            let dir = match &a.dir {
                Some(d) => d.clone(),
                None => setup::exe_dir()?,
            };
            return setup_then_launch(&rt, program, dir, a.no_start);
        }
        return rt.block_on(dispatch(cli.cmd));
    }
    let dir = setup::exe_dir()?;
    match setup::no_args_start(&dir, std::io::stdin().is_terminal()) {
        NoArgs::Launch(cfg) => launch(&rt, program, &cfg),
        NoArgs::Setup => {
            println!("No {} next to {}: starting the setup.", setup::CONFIG_FILE, dir.display());
            println!();
            setup_then_launch(&rt, program, dir, false)
        }
        NoArgs::Refuse(why) => anyhow::bail!(why),
    }
}

/// The interactive setup into `dir`; then, unless declined, the configuration it wrote.
fn setup_then_launch(rt: &tokio::runtime::Runtime, program: OsString, dir: std::path::PathBuf, no_start: bool) -> anyhow::Result<()> {
    std::fs::create_dir_all(&dir)?;
    let opts = SetupOptions::for_build(dir);
    match setup::run_setup(&mut StdConsole, &mut WrpcSetupNode { rt }, &opts)? {
        Some(cfg) if !no_start => {
            println!();
            launch(rt, program, &cfg)
        }
        _ => Ok(()),
    }
}

/// Runs the configuration at `cfg` as the command line it stands for (`setup::launch_args`), in the file's directory.
fn launch(rt: &tokio::runtime::Runtime, program: OsString, cfg: &std::path::Path) -> anyhow::Result<()> {
    let l = setup::launch_args(cfg)?;
    println!("Running {} (the same as):", cfg.display());
    for line in setup::command_line(&l, &setup::program_path()).lines() {
        println!("    {line}");
    }
    match l.role {
        LaunchRole::Run => {
            println!("The indexer first catches up with the chain (a new node's whole retention: up to some hours); the matcher and keepers start once it has.");
            if let Some(p) = &l.pause_file {
                println!("Pause trading without stopping: create the file {} (delete it to resume).", l.dir.join(p).display());
            }
        }
        LaunchRole::Index => println!("Index only: the indexer and its read API, no key."),
    }
    if let Some(a) = l.api {
        println!("Read API: http://{a}/v1/health");
    }
    println!("Stop it with Ctrl+C. A status line follows every minute.");
    println!();
    std::env::set_current_dir(&l.dir).map_err(|e| anyhow::anyhow!("cannot change to {}: {e}", l.dir.display()))?;
    let cli = Cli::try_parse_from(std::iter::once(program).chain(l.args.into_iter().map(OsString::from)))
        .map_err(|e| anyhow::anyhow!("the [launch] table of {} is not a valid command line:\n{e}", cfg.display()))?;
    rt.block_on(dispatch(cli.cmd))
}

async fn dispatch(cmd: Cmd) -> anyhow::Result<()> {
    match cmd {
        Cmd::Run(a) => executor::run(*a).await,
        Cmd::Index(a) => run_index(a).await,
        Cmd::Match(a) => run_match(a).await,
        Cmd::Keep(a) => run_keep(a).await,
        Cmd::X402(a) => x402::run(a).await,
        Cmd::Setup(_) => anyhow::bail!("setup runs before the runtime"),
    }
}
