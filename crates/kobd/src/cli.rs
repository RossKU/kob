//! `kobd`: the KOB daemon, one binary with two roles, the **Executor** and the **Indexer**.
//!
//! * A **start** names no command: `kobd` runs the configuration the setup wrote (`kobd.toml` next to the binary, or in
//!   `$KOBD_DIR`), or the interactive setup when there is none; `kobd --executor`, `kobd --indexer` and `kobd --executor
//!   --indexer` start those roles: with that configuration (which must have been set up for exactly these roles), with
//!   `--config <file>`, or from the options alone. Every option of the roles follows the flags (`kobd --executor --dry-run`,
//!   `kobd --indexer --listen 127.0.0.1:8091`; `kobd --help`). The Executor is the indexer, the matcher and the keepers in one
//!   process over one store (plus the x402 facilitator with `--x402-config`; docs/ops/executor.md), on lean storage; with
//!   `--indexer` too, over a full store with the history; the Indexer alone is the indexer and its read API (the public,
//!   keyless role).
//! * `setup`: the interactive first run (`kobd::setup`), which writes `kobd.toml` next to the binary;
//!   `setup --non-interactive` and `address-balance` are its JSON forms for a wrapper (the Android app, `kobd::app`);
//! * `db <tool>`: the tools of a data directory's database (migrate, replay, export-orders, export-book, import-orders, rebase);
//! * `logo-reports`: the administrator's side of the token-logo reports (list, restore, hide, export, import);
//! * `match`, `keep`: the matcher and the keepers alone, planning against a book snapshot file that an indexer exports
//!   (`--book-file`), for deployments that keep the key away from the public API;
//! * `x402`: the x402 facilitator alone (keyless; finality from the node's UTXO set);
//! * `withdraw`: sends KAS out of the operator key over the configured node, the public resolver pair included
//!   (`kobd::withdraw`).
//!
//! Roles that hold keys hold only the operator's hot key; the facilitator holds none.

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{anyhow, bail};

use crate::app::{self, AddressBalanceArgs, AppRole, AppSetup, RoleFlags};
use crate::executor::{self, RunArgs};
use crate::index_cli::{run_db, run_logo_reports, DbArgs, IndexOpts, LogoReportsArgs};
use crate::logging::{self, LogOptions};
use crate::matcher::cli::{run_keep, run_match, KeepArgs, MatchArgs};
use crate::setup::{self, NoArgs, SetupArgs, SetupOptions, StdConsole, WrpcSetupNode};
use crate::withdraw::{self, WithdrawArgs};
use crate::x402::{self, X402Args};
use clap::{Parser, Subcommand};

const ABOUT: &str = "kobd, the KOB daemon: the Executor (matcher, keepers, x402 facilitator) and the Indexer (full history, read API)";

const START_HELP: &str = "Start: `kobd` runs kobd.toml of the executable's directory ($KOBD_DIR when set), or the \
setup when there is none. `kobd --executor`, `kobd --indexer` or both start those roles: with that configuration (set up for \
exactly these roles), with --config <file>, or from the options alone; the options above follow the flags. The Indexer takes the \
indexer's options only (no --key-file, --dry-run, ...).

Commands: kobd setup, kobd address-balance, kobd withdraw, kobd db <migrate|replay|export-orders|export-book|import-orders|\
rebase>, kobd logo-reports, kobd match, kobd keep, kobd x402 (kobd help <command>).";

/// The log options of every start and command.
#[derive(clap::Args, Debug, Clone, Default)]
struct LogArgs {
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
}

/// A command (`kobd setup`, `kobd db migrate`, ...).
#[derive(Parser)]
#[command(
    name = "kobd",
    version = kob_protocol::VERSION,
    about = ABOUT,
    after_help = "A start names no command: `kobd`, `kobd --executor`, `kobd --indexer` (their options: `kobd --help`)."
)]
struct Cli {
    #[command(flatten)]
    log: LogArgs,
    #[command(subcommand)]
    cmd: Cmd,
}

/// A start of the Executor (or of both roles): every option of the role.
#[derive(Parser)]
#[command(name = "kobd", version = kob_protocol::VERSION, about = ABOUT, after_help = START_HELP, args_override_self = true)]
struct StartCli {
    #[command(flatten)]
    log: LogArgs,
    /// The roles to start: `--executor`, `--indexer` or both (one process; the public API and the signing key then live on the
    /// same machine). Without a flag the configuration's roles start.
    #[command(flatten)]
    roles: RoleFlags,
    #[command(flatten)]
    run: RunArgs,
}

/// A start of the Indexer alone: the indexer's options only.
#[derive(Parser)]
#[command(name = "kobd", version = kob_protocol::VERSION, about = ABOUT, after_help = START_HELP, args_override_self = true)]
struct StartIndexCli {
    #[command(flatten)]
    log: LogArgs,
    #[command(flatten)]
    roles: RoleFlags,
    #[command(flatten)]
    opts: IndexOpts,
}

#[derive(Subcommand)]
enum Cmd {
    /// The database of a data directory: migrate (bring an earlier build's store to this build's layout), replay (rebuild it
    /// from the record log), export-orders, export-book, import-orders, rebase.
    Db(DbArgs),
    /// Token-logo reports (`[logo_reports]`): review what was reported, restore a logo, hide one for good. Works next to a
    /// running executor of the same data directory (a short write to its database); there is no administrative route on the network.
    LogoReports(LogoReportsArgs),
    /// Batch-match crossing orders (spread + tips), trigger and arm stops in the batches, chain oversize crossings, from a
    /// book snapshot file.
    Match(MatchArgs),
    /// Refund / kill / close expired orders (paid by the orders' tips), sweep own strays, from a book snapshot file.
    Keep(KeepArgs),
    /// The x402 facilitator alone: GET /supported, POST /verify, POST /settle, GET /health, GET /metrics.
    X402(X402Args),
    /// First run: choose the roles (Executor, Indexer, or both), check the node, make the operator key, wait for funds, write
    /// kobd.toml next to the binary, start.
    Setup(SetupArgs),
    /// The operator's address and its plain balance from the configured node, as one JSON object (never the secret).
    AddressBalance(AddressBalanceArgs),
    /// Send KAS out of the operator key over the configured node (the public resolver pair included). Stop the executor first.
    Withdraw(WithdrawArgs),
}

/// What a start runs, with its options.
enum Start {
    Run(Box<RunArgs>),
    Index(Box<IndexOpts>),
}

impl Start {
    fn opts(&self) -> &IndexOpts {
        match self {
            Start::Run(a) => &a.index,
            Start::Index(o) => o,
        }
    }
}

/// The entry point of the `kobd` binary.
pub fn main() -> ExitCode {
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

/// The log options and their values, which may come before a command (`kobd --log-file x setup`).
const LOG_OPTIONS: [&str; 3] = ["--log-file", "--log-max-mb", "--log-files"];

/// The words of the command line other than the log options and their values.
fn without_log_options(args: &[OsString]) -> Vec<&OsString> {
    let mut out = vec![];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let s = a.to_string_lossy();
        if LOG_OPTIONS.contains(&s.as_ref()) {
            it.next();
        } else if !LOG_OPTIONS.iter().any(|o| s.starts_with(&format!("{o}="))) {
            out.push(a);
        }
    }
    out
}

/// Whether the command line names a command (`kobd setup ...`) rather than a start (options only, or nothing).
fn names_a_command(args: &[OsString]) -> bool {
    without_log_options(args).first().is_some_and(|a| !a.to_string_lossy().starts_with('-'))
}

/// Whether a start names nothing but roles (and log options): `kobd`, `kobd --executor`, ...
fn only_roles(args: &[OsString]) -> bool {
    without_log_options(args).iter().all(|a| matches!(a.to_string_lossy().as_ref(), "--executor" | "--indexer"))
}

fn start_on(rt: &tokio::runtime::Runtime, argv: Vec<OsString>) -> anyhow::Result<()> {
    let program = argv.first().cloned().unwrap_or_else(|| "kobd".into());
    let args: Vec<OsString> = argv.iter().skip(1).cloned().collect();
    if names_a_command(&args) {
        return command(rt, program, Cli::parse_from(argv));
    }
    // a start: the options are checked (and --help, --version answered) before anything else happens
    let cli = StartCli::parse_from(&argv);
    logging::init(false);
    start_roles(rt, program, &args, &cli)
}

/// A command.
fn command(rt: &tokio::runtime::Runtime, program: OsString, cli: Cli) -> anyhow::Result<()> {
    // the JSON commands keep stdout for their one object: the log goes to stderr
    let json_out = matches!(&cli.cmd, Cmd::AddressBalance(_) | Cmd::Setup(SetupArgs { non_interactive: true, .. }));
    logging::init(json_out);
    if !json_out {
        let config = match &cli.cmd {
            Cmd::Db(a) => a.opts.config.as_deref(),
            Cmd::LogoReports(a) => a.opts.config.as_deref(),
            _ => None,
        };
        attach_log(&cli.log, config)?;
    }
    match cli.cmd {
        Cmd::Setup(a) => {
            let dir = match &a.dir {
                Some(d) => d.clone(),
                None => setup::home_dir()?,
            };
            if a.non_interactive {
                return setup_json(dir, &a);
            }
            setup_then_launch(rt, program, dir, a.no_start, a.roles.role())
        }
        Cmd::AddressBalance(a) => {
            let dir = match &a.dir {
                Some(d) => d.clone(),
                None => setup::home_dir()?,
            };
            println!("{}", app::address_balance_live(&dir, rt)?);
            Ok(())
        }
        Cmd::Withdraw(a) => withdraw_cmd(rt, &a),
        Cmd::Db(a) => rt.block_on(run_db(a)),
        Cmd::LogoReports(a) => run_logo_reports(a),
        Cmd::Match(a) => rt.block_on(run_match(a)),
        Cmd::Keep(a) => rt.block_on(run_keep(a)),
        Cmd::X402(a) => rt.block_on(x402::run(a)),
    }
}

/// A start: the configuration (`--config`, else `kobd.toml` of the folder) or the options alone. `args` are the words of
/// the command line without the program name, `cli` their parse.
fn start_roles(rt: &tokio::runtime::Runtime, program: OsString, args: &[OsString], cli: &StartCli) -> anyhow::Result<()> {
    let want = cli.roles.role();
    let path = match &cli.run.index.config {
        Some(p) => p.clone(),
        None => {
            let dir = setup::home_dir()?;
            let p = dir.join(setup::CONFIG_FILE);
            if !p.is_file() {
                if !only_roles(args) {
                    // options and no configuration: the roles named start from the options alone
                    let want = want.ok_or_else(|| {
                        anyhow!(
                            "no {} in {}: name the roles to start from the options alone (`kobd --executor ...`, `kobd --indexer \
                             ...`), or run `kobd setup`",
                            setup::CONFIG_FILE,
                            dir.display()
                        )
                    })?;
                    let start = parse_start(want, &program, args)?;
                    return run_start(rt, start);
                }
                return match setup::no_args_start(&dir, std::io::stdin().is_terminal()) {
                    NoArgs::Refuse(why) => bail!(why),
                    _ => {
                        println!("No {} in {}: starting the setup.", setup::CONFIG_FILE, dir.display());
                        println!();
                        setup_then_launch(rt, program, dir, false, want)
                    }
                };
            }
            p
        }
    };
    let cfg = crate::config::IndexerConfig::load(&path)?;
    match AppRole::of_config(&cfg) {
        Some(have) => {
            if let Some(want) = want.filter(|w| *w != have) {
                bail!(
                    "{} is set up for the {} but the command line asks for the {}: start it without role flags, or run `kobd setup` to change the roles",
                    path.display(),
                    have.display(),
                    want.display()
                );
            }
            launch(rt, program, &path, args)
        }
        None => {
            // a plain --config file: the roles come from the command line
            let want = want.ok_or_else(|| {
                anyhow!(
                    "{} has no [launch] table, so it names no roles: start `kobd --executor --config ...` or `kobd --indexer --config \
                     ...`, or run `kobd setup`",
                    path.display()
                )
            })?;
            let start = parse_start(want, &program, args)?;
            run_start(rt, start)
        }
    }
}

/// The start of `roles` with the words `args` (without the program name): the Executor's options, or the indexer's for the
/// Indexer alone. The roles settle the storage: the Executor alone is lean, with the Indexer too it keeps the full store.
fn parse_start(roles: AppRole, program: &OsString, args: &[OsString]) -> anyhow::Result<(Start, LogArgs)> {
    let argv = std::iter::once(program.clone()).chain(args.iter().cloned());
    match roles {
        AppRole::Indexer => {
            let c = StartIndexCli::try_parse_from(argv)
                .map_err(|e| anyhow!("the Indexer takes the indexer's options only (`kobd --help`): {}", e.render()))?;
            if c.opts.lean {
                bail!("--lean: the Indexer keeps the full history; lean storage is the Executor's (`kobd --executor`)");
            }
            Ok((Start::Index(Box::new(c.opts)), c.log))
        }
        roles => {
            let mut c = StartCli::try_parse_from(argv).map_err(|e| anyhow!("{}", e.render()))?;
            let o = &mut c.run.index;
            // fast start follows the role as lean storage does, and as the setup's kobd.toml writes it: on for every start of
            // the Executor (alone or with the Indexer), from options alone too, unless --no-fast-start or the configuration's own
            // `fast_start` key says otherwise (it does nothing on a data directory that has a store already)
            if !o.fast_start && !o.no_fast_start && !config_sets_fast_start(o.config.as_deref()) {
                o.fast_start = true;
            }
            if roles == AppRole::Executor {
                if o.no_lean {
                    bail!("--no-lean: the Executor alone keeps lean storage; a full store with the history is `kobd --executor --indexer`");
                }
                (o.lean, o.no_lean) = (true, false);
            } else {
                if o.lean {
                    bail!("--lean: the Executor and Indexer keeps the full store; lean storage is the Executor alone (`kobd --executor`)");
                }
                (o.lean, o.no_lean) = (false, true);
            }
            Ok((Start::Run(Box::new(c.run)), c.log))
        }
    }
}

/// Whether the configuration file at `path` has a `fast_start` key (either value): then the file decides (`build_config` reads
/// it), not the role. A file that cannot be read says nothing here (the start reports it).
fn config_sets_fast_start(path: Option<&Path>) -> bool {
    path.and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| t.parse::<toml::Table>().ok())
        .is_some_and(|t| t.contains_key("fast_start"))
}

/// Runs a start that is not a launch configuration's (no banner, but the one line that says whether trading waits for the
/// indexer to catch up: a service started from options alone would otherwise give no hint why it plans nothing for hours).
fn run_start(rt: &tokio::runtime::Runtime, (start, log): (Start, LogArgs)) -> anyhow::Result<()> {
    refuse_second_writer(&start)?;
    attach_log(&log, start.opts().config.as_deref())?;
    if let Start::Run(a) = &start {
        let note = start_note(a);
        if !note.is_empty() {
            tracing::info!("{note}");
        }
    }
    rt.block_on(dispatch(start))
}

/// `setup --non-interactive`: the files, and one JSON object on stdout.
fn setup_json(dir: std::path::PathBuf, a: &SetupArgs) -> anyhow::Result<()> {
    let Some(node) = a.node.clone() else {
        bail!("--non-interactive needs --node resolver|<ws-url> and a role: --executor, --indexer or both");
    };
    let Some(role) = a.roles.role() else { bail!("--non-interactive needs a role: --executor, --indexer or both") };
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
fn setup_then_launch(
    rt: &tokio::runtime::Runtime,
    program: OsString,
    dir: std::path::PathBuf,
    no_start: bool,
    roles: Option<AppRole>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&dir)?;
    // the dialogue stays clean: the log lines of the node checks go to the log file only (the one a run appends to), and the
    // console shows the log again for what follows
    let console_was = logging::console_shows_log();
    if logging::attach_file(LogOptions::new(dir.join(setup::LOG_FILE), 10, 5)).is_ok() {
        logging::set_console(false);
    }
    let opts = SetupOptions::for_build(dir);
    let done = setup::run_setup_with(&mut StdConsole, &mut WrpcSetupNode { rt }, &opts, roles, no_start);
    logging::set_console(console_was);
    match done? {
        Some(cfg) => {
            println!();
            launch(rt, program, &cfg, &[])
        }
        _ => Ok(()),
    }
}

/// The words of a command line without `--config` and its value (a launch names its file itself) and without the role flags
/// (the launch names the file's, which the command line's were checked against).
fn without_config(args: &[OsString]) -> Vec<OsString> {
    let mut out = vec![];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let s = a.to_string_lossy();
        if s == "--config" {
            it.next();
        } else if !s.starts_with("--config=") && s != "--executor" && s != "--indexer" {
            out.push(a.clone());
        }
    }
    out
}

/// Runs the configuration at `cfg` (`setup::launch_args`) in the file's directory, with the options `args` of the command line
/// after the file's (a later option wins).
fn launch(rt: &tokio::runtime::Runtime, program: OsString, cfg: &Path, args: &[OsString]) -> anyhow::Result<()> {
    let l = setup::launch_args(cfg)?;
    // read while the working directory is still the caller's (`cfg` may be a relative path)
    let address = app::operator_address(cfg);
    // the options and the data directory first: a second instance is refused before it prints a banner or touches the log
    std::env::set_current_dir(&l.dir).map_err(|e| anyhow!("cannot change to {}: {e}", l.dir.display()))?;
    let mut words: Vec<OsString> = l.args.iter().map(OsString::from).collect();
    words.extend(without_config(args));
    let (start, log) = parse_start(l.roles, &program, &words)
        .map_err(|e| anyhow!("the start options of {} ([launch] and the command line): {e}", cfg.display()))?;
    refuse_second_writer(&start)?;
    println!("Roles: {}.", l.roles.display());
    println!("Running {} (the same as):", cfg.display());
    let shown = setup::Launch { args: words.iter().map(|w| w.to_string_lossy().into_owned()).collect(), ..l.clone() };
    for line in setup::command_line(&shown, &setup::program_path()).lines() {
        println!("    {line}");
    }
    match &start {
        Start::Run(a) => {
            println!("{}", start_note(a));
            if let Some(p) = &a.pause_file {
                println!("Pause trading without stopping: create the file {} (delete it to resume).", l.dir.join(p).display());
            }
        }
        Start::Index(_) => println!("The Indexer follows the chain and serves the read API; it has no key."),
    }
    if let Some(a) = address {
        println!("Funding address (send KAS here; `{} address-balance` shows it and its balance): {a}", setup::program_path());
    }
    if let Ok(c) = crate::index_cli::build_config(start.opts()) {
        if c.api.enabled {
            println!("Read API: http://{}/v1/health", c.api.listen);
        }
    }
    match start.opts().status_every() {
        Some(every) => println!("Stop it with Ctrl+C. A status line follows every {} s.", every.as_secs()),
        None => println!("Stop it with Ctrl+C."),
    }
    println!();
    attach_log(&log, start.opts().config.as_deref())?;
    rt.block_on(dispatch(start))
}

/// Refuses a start that writes a data directory another kobd process holds, before anything is printed or written to the log
/// file (the running process has it open, and it may be the same file).
fn refuse_second_writer(start: &Start) -> anyhow::Result<()> {
    // a configuration that does not build is reported by the start itself
    if let Ok(cfg) = crate::index_cli::build_config(start.opts()) {
        crate::indexer::lock::WriterLock::check(&cfg)?;
    }
    Ok(())
}

/// What the banner says about the start of the Executor: whether trading waits for the history (it does not with the fast start).
fn start_note(a: &RunArgs) -> String {
    use crate::faststart::Plan;
    let Ok(cfg) = crate::index_cli::build_config(&a.index) else { return String::new() };
    // the facilitator's book is bound to one store: no fast start with it (a dry run serves no facilitator)
    let x402 = a.x402().is_some();
    let plan = if x402 { Plan::Normal } else { crate::faststart::plan(&cfg) };
    match plan {
        Plan::Fresh | Plan::Resume => "Fast start: the matcher and keepers start within seconds, on the orders seen from now on. The history is indexed in the background (up to some hours on a new node), and the executor switches to the full book by itself.".into(),
        Plan::Normal if cfg.db_path().exists() => "The indexer resumes from its stored position; the matcher and keepers start once it has caught up (usually seconds).".into(),
        Plan::Normal => {
            let why = if x402 {
                " (no fast start: the x402 facilitator is served)"
            } else if !cfg.fast_start {
                " (fast start is off)"
            } else {
                ""
            };
            format!("The indexer first catches up with the chain (the node's whole retention: up to some hours){why}; the matcher and keepers start once it has.")
        }
    }
}

/// The log file of the command line (`--log-file`) or of the configuration `cfg_path` (`log_file`, relative to the
/// configuration's directory), attached to the log with the rotation settings; the console says so when it still shows the log.
fn attach_log(log: &LogArgs, cfg_path: Option<&Path>) -> anyhow::Result<()> {
    let cfg = cfg_path.and_then(|p| crate::config::IndexerConfig::load(p).ok());
    let base = cfg_path.and_then(|p| p.parent()).filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let file = match (&log.log_file, cfg.as_ref().and_then(|c| c.log_file.as_ref())) {
        (Some(f), _) => f.clone(),
        (None, Some(f)) => logging::resolve(base, f),
        (None, None) => return Ok(()),
    };
    let max_mb = log.log_max_mb.or(cfg.as_ref().map(|c| c.log_max_mb)).unwrap_or(10);
    let keep = log.log_files.or(cfg.as_ref().map(|c| c.log_files)).unwrap_or(5);
    logging::attach_file(LogOptions::new(file.clone(), max_mb, keep))
        .map_err(|e| anyhow!("cannot open the log file {}: {e}", file.display()))?;
    if logging::console_shows_log() {
        println!("Log file: {} (a new file at {max_mb} MB, {keep} old ones kept)", file.display());
    }
    Ok(())
}

async fn dispatch(start: Start) -> anyhow::Result<()> {
    match start {
        Start::Run(a) => executor::run(*a).await,
        Start::Index(o) => crate::index_cli::follow_and_serve(*o).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &[&str]) -> Vec<OsString> {
        s.iter().map(OsString::from).collect()
    }

    #[test]
    fn a_command_is_told_from_a_start() {
        assert!(names_a_command(&words(&["setup", "--executor"])));
        assert!(names_a_command(&words(&["--log-file", "x.log", "db", "migrate"])));
        assert!(names_a_command(&words(&["--log-files=3", "withdraw"])));
        assert!(!names_a_command(&words(&[])));
        assert!(!names_a_command(&words(&["--executor", "--dry-run"])));
        assert!(!names_a_command(&words(&["--log-file", "setup"])), "a value of a log option is not a command");
        assert!(!names_a_command(&words(&["--help"])));
        assert!(only_roles(&words(&["--executor", "--indexer", "--log-file", "x.log"])));
        assert!(!only_roles(&words(&["--executor", "--dry-run"])));
        assert_eq!(
            without_config(&words(&["--executor", "--config", "a.toml", "--dry-run", "--config=b.toml", "--listen", "127.0.0.1:1"])),
            words(&["--dry-run", "--listen", "127.0.0.1:1"])
        );
    }

    #[test]
    fn the_roles_settle_the_storage_and_the_indexer_takes_its_options_only() {
        let p = OsString::from("kobd");
        let run = |r: AppRole, a: &[&str]| match parse_start(r, &p, &words(a)) {
            Ok((Start::Run(a), _)) => Ok((a.index.lean, a.index.no_lean, a.dry_run, a.index.fast_start)),
            Ok((Start::Index(_), _)) => Err("index".to_string()),
            Err(e) => Err(e.to_string()),
        };
        // fast start follows the role too (as `kobd setup` writes it), from options alone
        assert_eq!(run(AppRole::Executor, &["--executor", "--dry-run"]), Ok((true, false, true, true)));
        assert_eq!(run(AppRole::Executor, &["--lean"]), Ok((true, false, false, true)));
        assert!(run(AppRole::Executor, &["--no-lean"]).unwrap_err().contains("kobd --executor --indexer"));
        assert_eq!(run(AppRole::ExecutorIndexer, &[]), Ok((false, true, false, true)));
        assert_eq!(run(AppRole::Executor, &["--no-fast-start"]), Ok((true, false, false, false)), "--no-fast-start turns it off");
        // a configuration's own `fast_start` key decides (either way); one without the key gets the role's
        let d = tempfile::tempdir().unwrap();
        let (off, plain) = (d.path().join("off.toml"), d.path().join("plain.toml"));
        std::fs::write(
            &off,
            "network = \"testnet-10\"
fast_start = false
",
        )
        .unwrap();
        std::fs::write(
            &plain,
            "network = \"testnet-10\"
",
        )
        .unwrap();
        let with = |f: &std::path::Path| run(AppRole::Executor, &["--config", f.to_str().unwrap()]).map(|r| r.3);
        assert_eq!((with(&off), with(&plain)), (Ok(false), Ok(true)));
        assert!(run(AppRole::ExecutorIndexer, &["--lean"]).unwrap_err().contains("lean storage is the Executor alone"));
        // a later option wins (the command line after the configuration's [launch] options)
        let Ok((Start::Run(a), _)) =
            parse_start(AppRole::Executor, &p, &words(&["--listen", "127.0.0.1:1", "--listen", "127.0.0.1:2"]))
        else {
            panic!("a start")
        };
        assert_eq!(a.index.listen, Some("127.0.0.1:2".parse().unwrap()));
        // the Indexer: no option of the Executor, no lean storage
        assert!(matches!(
            parse_start(AppRole::Indexer, &p, &words(&["--indexer", "--listen", "127.0.0.1:1"])),
            Ok((Start::Index(_), _))
        ));
        let e = parse_start(AppRole::Indexer, &p, &words(&["--dry-run"])).err().unwrap().to_string();
        assert!(e.contains("the Indexer takes the indexer's options only") && e.contains("--dry-run"), "{e}");
        assert!(parse_start(AppRole::Indexer, &p, &words(&["--lean"])).is_err());
    }
}
