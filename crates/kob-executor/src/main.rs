//! `kob-executor`: one binary with role subcommands.
//!
//! * `run`: the indexer, the matcher and the keepers in one process over one store, plus the x402
//!   facilitator with `--x402-config` (the operator's default; docs/ops/executor.md);
//! * `index`: the indexer and its read API alone (the public, keyless role);
//! * `match`, `keep`: the matcher and the keepers alone, planning against a book snapshot file that
//!   an indexer exports (`--book-file`), for deployments that keep the key away from the public API;
//! * `x402`: the x402 facilitator alone (keyless; finality from the node's UTXO set);
//! * `setup`: the interactive first run (`kob_executor::setup`), which writes `kob-executor.toml` next to the binary;
//!   `setup --non-interactive` and `address-balance` are its JSON forms for a wrapper (the Android app, `kob_executor::app`);
//! * `withdraw`: sends KAS out of the operator key over the configured node, the public resolver pair included (`kob_executor::withdraw`).
//!
//! Without arguments (a double click) the binary runs `kob-executor.toml` from its own directory, or the setup when there is
//! none (docs/ops/executor-quickstart.md).
//!
//! Roles that hold keys hold only the operator's hot key; the facilitator holds none.

use std::ffi::OsString;
use std::io::IsTerminal;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use kob_executor::app::{self, AddressBalanceArgs, AppSetup};
use kob_executor::config::LaunchRole;
use kob_executor::executor::{self, RunArgs};
use kob_executor::index_cli::{run_index, IndexArgs};
use kob_executor::logging::{self, LogOptions};
use kob_executor::matcher::cli::{run_keep, run_match, KeepArgs, MatchArgs};
use kob_executor::setup::{self, NoArgs, SetupArgs, SetupOptions, StdConsole, WrpcSetupNode};
use kob_executor::withdraw::{self, WithdrawArgs};
use kob_executor::x402::{self, X402Args};

#[derive(Parser)]
#[command(
    name = "kob-executor",
    version = kob_protocol::VERSION,
    about = "KOB executor: indexer, matcher, keepers and x402 facilitator",
    after_help = "Without arguments: runs kob-executor.toml from the executable's directory ($KOB_EXECUTOR_DIR when set), or the setup when there is none."
)]
struct Cli {
    /// Write the log to this file (rotated by size) instead of only the console; the console also shows it while it is a
    /// terminal. `log_file` of the configuration does the same.
    #[arg(long, global = true, env = "KOB_LOG_FILE", value_name = "PATH")]
    log_file: Option<std::path::PathBuf>,
    /// Rotate the log file at this many MB (default 10, or `log_max_mb` of the configuration; 0: never).
    #[arg(long, global = true, value_name = "MB")]
    log_max_mb: Option<u64>,
    /// Rotated log files kept (default 5, or `log_files` of the configuration; 0: the file starts over when it is full).
    #[arg(long, global = true, value_name = "N")]
    log_files: Option<usize>,
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
    /// The operator's address and its plain balance from the configured node, as one JSON object (never the secret).
    AddressBalance(AddressBalanceArgs),
    /// Send KAS out of the operator key over the configured node (the public resolver pair included). Stop the executor first.
    Withdraw(WithdrawArgs),
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
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let res = start_on(&rt, argv);
    // dropping the runtime waits for every blocking call still running (a database read, a lookup at a resolver): a stop does
    // not wait for them past this
    rt.shutdown_timeout(std::time::Duration::from_secs(3));
    res
}

fn start_on(rt: &tokio::runtime::Runtime, argv: Vec<OsString>) -> anyhow::Result<()> {
    let program = argv.first().cloned().unwrap_or_else(|| "kob-executor".into());
    if argv.len() > 1 {
        let cli = Cli::parse_from(argv);
        // the JSON commands keep stdout for their one object: the log goes to stderr
        let json_out = matches!(&cli.cmd, Cmd::AddressBalance(_) | Cmd::Setup(SetupArgs { non_interactive: true, .. }));
        logging::init(json_out);
        if !json_out {
            refuse_second_writer(&cli.cmd)?;
            attach_log(&cli)?;
        }
        match &cli.cmd {
            Cmd::Setup(a) => {
                let dir = match &a.dir {
                    Some(d) => d.clone(),
                    None => setup::home_dir()?,
                };
                if a.non_interactive {
                    return setup_json(dir, a);
                }
                return setup_then_launch(rt, program, dir, a.no_start);
            }
            Cmd::AddressBalance(a) => {
                let dir = match &a.dir {
                    Some(d) => d.clone(),
                    None => setup::home_dir()?,
                };
                println!("{}", app::address_balance_live(&dir, rt)?);
                return Ok(());
            }
            Cmd::Withdraw(a) => return withdraw_cmd(rt, a),
            _ => {}
        }
        return rt.block_on(dispatch(cli.cmd));
    }
    logging::init(false);
    let dir = setup::home_dir()?;
    match setup::no_args_start(&dir, std::io::stdin().is_terminal()) {
        NoArgs::Launch(cfg) => launch(rt, program, &cfg),
        NoArgs::Setup => {
            println!("No {} in {}: starting the setup.", setup::CONFIG_FILE, dir.display());
            println!();
            setup_then_launch(rt, program, dir, false)
        }
        NoArgs::Refuse(why) => anyhow::bail!(why),
    }
}

/// `setup --non-interactive`: the files, and one JSON object on stdout.
fn setup_json(dir: std::path::PathBuf, a: &SetupArgs) -> anyhow::Result<()> {
    let (Some(node), Some(role)) = (a.node.clone(), a.role) else {
        anyhow::bail!("--non-interactive needs --node and --role");
    };
    let opts = SetupOptions::for_build(dir);
    let ask = AppSetup { node, role, new_key: a.new_key, network: a.network.clone() };
    println!("{}", app::setup_non_interactive(&opts, &ask)?);
    Ok(())
}

/// `withdraw`: the withdrawal over the configured node, the confirmation on the terminal.
fn withdraw_cmd(rt: &tokio::runtime::Runtime, a: &WithdrawArgs) -> anyhow::Result<()> {
    let (cfg, base) = withdraw::config_location(a)?;
    let req = withdraw::Request {
        to: a.to.clone(),
        amount: withdraw::parse_amount(&a.amount)?,
        key_file: a.key_file.clone(),
        fee_rate: a.fee_rate,
        yes: a.yes,
        interactive: std::io::stdin().is_terminal(),
        dry_run: a.dry_run,
    };
    withdraw::withdraw(&cfg, &base, &req, &mut withdraw::WrpcWithdrawNode::new(rt), &mut StdConsole)?;
    Ok(())
}

/// The interactive setup into `dir`; then, unless declined, the configuration it wrote.
fn setup_then_launch(rt: &tokio::runtime::Runtime, program: OsString, dir: std::path::PathBuf, no_start: bool) -> anyhow::Result<()> {
    std::fs::create_dir_all(&dir)?;
    // the dialogue stays clean: the log lines of the node checks go to the log file only (the one a run appends to), and the
    // console shows the log again for what follows
    let console_was = logging::console_shows_log();
    if logging::attach_file(LogOptions::new(dir.join(setup::LOG_FILE), 10, 5)).is_ok() {
        logging::set_console(false);
    }
    let opts = SetupOptions::for_build(dir);
    let done = setup::run_setup(&mut StdConsole, &mut WrpcSetupNode { rt }, &opts);
    logging::set_console(console_was);
    match done? {
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
    // read while the working directory is still the caller's (`cfg` may be a relative path)
    let address = app::operator_address(cfg);
    // the command line and the data directory first: a second instance is refused before it prints a banner or touches the log
    std::env::set_current_dir(&l.dir).map_err(|e| anyhow::anyhow!("cannot change to {}: {e}", l.dir.display()))?;
    let cli = Cli::try_parse_from(std::iter::once(program).chain(l.args.iter().map(OsString::from)))
        .map_err(|e| anyhow::anyhow!("the [launch] table of {} is not a valid command line:\n{e}", cfg.display()))?;
    refuse_second_writer(&cli.cmd)?;
    println!("Running {} (the same as):", cfg.display());
    for line in setup::command_line(&l, &setup::program_path()).lines() {
        println!("    {line}");
    }
    match l.role {
        LaunchRole::Run => {
            println!("{}", start_note(&cli.cmd));
            if let Some(p) = &l.pause_file {
                println!("Pause trading without stopping: create the file {} (delete it to resume).", l.dir.join(p).display());
            }
        }
        LaunchRole::Index => println!("Index only: the indexer and its read API, no key."),
    }
    if let Some(a) = address {
        println!("Funding address (send KAS here; `{} address-balance` shows it and its balance): {a}", setup::program_path());
    }
    if let Some(a) = l.api {
        println!("Read API: http://{a}/v1/health");
    }
    println!("Stop it with Ctrl+C. A status line follows every minute.");
    println!();
    attach_log(&cli)?;
    rt.block_on(dispatch(cli.cmd))
}

/// Refuses a command that writes a data directory another kob-executor process holds, before anything is printed or written
/// to the log file (the running process has it open, and it may be the same file).
fn refuse_second_writer(cmd: &Cmd) -> anyhow::Result<()> {
    let opts = match cmd {
        Cmd::Run(a) => &a.index,
        Cmd::Index(a) if matches!(a.sub, None | Some(kob_executor::index_cli::IndexSub::Run)) => &a.opts,
        _ => return Ok(()),
    };
    // a configuration that does not build is reported by the command itself
    if let Ok(cfg) = kob_executor::index_cli::build_config(opts) {
        kob_executor::indexer::lock::WriterLock::check(&cfg)?;
    }
    Ok(())
}

/// What the banner says about the start of `run`: whether trading waits for the history (it does not with the fast start).
fn start_note(cmd: &Cmd) -> String {
    use kob_executor::faststart::Plan;
    let Cmd::Run(a) = cmd else { return String::new() };
    let Ok(cfg) = kob_executor::index_cli::build_config(&a.index) else { return String::new() };
    // the facilitator's book is bound to one store: no fast start with it
    let plan = if a.x402_config.is_some() { Plan::Normal } else { kob_executor::faststart::plan(&cfg) };
    match plan {
        Plan::Fresh | Plan::Resume => "Fast start: the matcher and keepers start within seconds, on the orders seen from now on. The history is indexed in the background (up to some hours on a new node), and the executor switches to the full book by itself.".into(),
        Plan::Normal if cfg.db_path().exists() => "The indexer resumes from its stored position; the matcher and keepers start once it has caught up (usually seconds).".into(),
        Plan::Normal => "The indexer first catches up with the chain (a new node's whole retention: up to some hours); the matcher and keepers start once it has.".into(),
    }
}

/// The configuration file a command reads (`--config`), if it takes one.
fn config_of(cmd: &Cmd) -> Option<&std::path::Path> {
    match cmd {
        Cmd::Run(a) => a.index.config.as_deref(),
        Cmd::Index(a) => a.opts.config.as_deref(),
        _ => None,
    }
}

/// The log file of the command line (`--log-file`) or of the configuration (`log_file`, relative to the configuration's
/// directory), attached to the log with the rotation settings; the console says so when it still shows the log.
fn attach_log(cli: &Cli) -> anyhow::Result<()> {
    let cfg_path = config_of(&cli.cmd);
    let cfg = cfg_path.and_then(|p| kob_executor::config::IndexerConfig::load(p).ok());
    let base = cfg_path.and_then(|p| p.parent()).filter(|d| !d.as_os_str().is_empty()).unwrap_or(std::path::Path::new("."));
    let file = match (&cli.log_file, cfg.as_ref().and_then(|c| c.log_file.as_ref())) {
        (Some(f), _) => f.clone(),
        (None, Some(f)) => logging::resolve(base, f),
        (None, None) => return Ok(()),
    };
    let max_mb = cli.log_max_mb.or(cfg.as_ref().map(|c| c.log_max_mb)).unwrap_or(10);
    let keep = cli.log_files.or(cfg.as_ref().map(|c| c.log_files)).unwrap_or(5);
    logging::attach_file(LogOptions::new(file.clone(), max_mb, keep))
        .map_err(|e| anyhow::anyhow!("cannot open the log file {}: {e}", file.display()))?;
    if logging::console_shows_log() {
        println!("Log file: {} (a new file at {max_mb} MB, {keep} old ones kept)", file.display());
    }
    Ok(())
}

async fn dispatch(cmd: Cmd) -> anyhow::Result<()> {
    match cmd {
        Cmd::Run(a) => executor::run(*a).await,
        Cmd::Index(a) => run_index(a).await,
        Cmd::Match(a) => run_match(a).await,
        Cmd::Keep(a) => run_keep(a).await,
        Cmd::X402(a) => x402::run(a).await,
        Cmd::Setup(_) | Cmd::AddressBalance(_) | Cmd::Withdraw(_) => anyhow::bail!("this command runs before the runtime"),
    }
}
