//! The `kobd` command line: one binary with two roles, the Executor and the Indexer.
//!
//! A start names no command. `kobd` runs the configuration the setup wrote (`kobd.toml` next to
//! the binary, or in `$KOBD_DIR`), or the interactive setup when there is none. `kobd --executor`,
//! `kobd --indexer` and `kobd --executor --indexer` start those roles. They use that
//! configuration, which must have been set up for exactly these roles, or `--config <file>`, or
//! the options alone. The options of the roles follow the flags, see `kobd --executor --help` and
//! `kobd --indexer --help`.
//!
//! The Executor is the indexer, the matcher and the keepers in one process over one store, on lean
//! storage. With `--x402-config` it also runs the x402 facilitator, see docs/ops/executor.md. With
//! `--indexer` too it keeps a full store with the history. The Indexer alone is the indexer and
//! its read API, a public role with no key.
//!
//! # Commands
//!
//! - `setup`: the interactive first run (`kobd::setup`), which writes `kobd.toml` next to the
//!   binary. `setup --non-interactive` and `address-balance` are its JSON forms for a wrapper such
//!   as the Android app (`kobd::app`).
//! - `config check`: the configuration read offline, with what a start would use.
//! - `db <tool>`: the tools of a data directory's database (info, migrate, vacuum, replay,
//!   export-orders, export-book, import-orders, rebase).
//! - `logo-reports`: the administrator's side of the token-logo reports (list, restore, hide,
//!   export, import).
//! - `match`, `keep`: the matcher and the keepers alone. They plan against a book snapshot file
//!   that an indexer exports (`--book-file`), for deployments that keep the key away from the
//!   public API.
//! - `x402`: the x402 facilitator alone, with no key. Finality comes from the UTXO set of the node.
//! - `withdraw`: sends KAS out of the operator key over the configured node, the public resolver
//!   pair included (`kobd::withdraw`).
//! - `health`: asks the read API of a running kobd whether it is healthy (`GET /v1/health`), for
//!   a container's `HEALTHCHECK`.
//!
//! Roles that hold keys hold only the operator's hot key. The facilitator holds none.
//!
//! # Exit status
//!
//! - 0: a stop.
//! - 1: an error. A supervisor starts the process again.
//! - 2: a usage mistake ([`Usage`], and clap's own), a configuration file that is missing or does
//!   not parse, or a stop that did not finish in time (`shutdown`).
//! - 130: a second Ctrl+C.
//!
//! Commands that print a document on stdout keep the log on stderr.

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
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
use clap::{CommandFactory, Parser, Subcommand};

const ABOUT: &str = "kobd, the KOB daemon: the Executor (matcher, keepers, x402 facilitator) and the Indexer (full history, read API)";

/// The top-level usage: the three kinds of start and the commands.
const USAGE: &str = "kobd                        run kobd.toml of this folder (or the setup when there is none)
       kobd --executor [OPTIONS]   start the Executor (its options: kobd --executor --help)
       kobd --indexer [OPTIONS]    start the Indexer (its options: kobd --indexer --help)
       kobd <COMMAND> [OPTIONS]    a command (kobd help <command>)";

const TOP_HELP: &str = "A start names no command: `kobd` runs kobd.toml of the executable's directory ($KOBD_DIR when set), or \
the setup when there is none. `kobd --executor`, `kobd --indexer` or both start those roles: with that configuration (set up for \
exactly these roles), with --config <file>, or from the options alone. Their options: `kobd --executor --help`, `kobd --indexer \
--help`.

Exit status: 0 stopped, 1 an error (a supervisor may start it again), 2 a usage mistake, a configuration file that is \
missing or does not parse, or a stop that did not finish in time (do not start it again unchanged), 130 a second Ctrl+C.";

const START_HELP: &str = "A start: `kobd` runs kobd.toml of the executable's directory ($KOBD_DIR when set), or the setup when \
there is none; `kobd --executor`, `kobd --indexer` or both start those roles with that configuration (set up for exactly these \
roles), with --config <file>, or from the options alone; the options above follow the flags. The node is --rpc-url (default: \
the public pair, `resolver`); --extra-node adds a further one. The commands (setup, withdraw, db, config check, ...): `kobd --help`.";

const INDEX_HELP: &str = "The Indexer alone keeps the full history and serves the read API; it has no key (no --key-file, \
--dry-run, ...: those are the Executor's, `kobd --executor --help`). The node is --rpc-url (default: the public pair, \
`resolver`); --extra-node adds a further one. The commands: `kobd --help`.";

/// A mistake on the command line, such as an option of another role, the retired `--node` option or
/// a role the configuration was not set up for.
///
/// It exits with status 2, like clap's own usage errors, so that a supervisor does not start the
/// same command again. A configuration file that is missing or does not parse
/// (`config::ConfigError`) exits with 2 too.
#[derive(Debug)]
pub struct Usage(pub String);

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Usage {}

/// A usage error ([`Usage`]) as an `anyhow` error.
fn usage(msg: impl Into<String>) -> anyhow::Error {
    Usage(msg.into()).into()
}

/// The log options of every start and command.
#[derive(clap::Args, Debug, Clone, Default)]
#[command(next_help_heading = "Log")]
struct LogArgs {
    /// Also write the log to this file, rotated by size (the console shows it too: a terminal, a
    /// pipe, the journal). `log_file` of the configuration does the same.
    #[arg(long, global = true, env = "KOB_LOG_FILE", value_name = "PATH")]
    log_file: Option<std::path::PathBuf>,
    /// Rotate the log file at this many MB (default 10, or `log_max_mb` of the configuration; 0:
    /// never).
    #[arg(long, global = true, value_name = "MB")]
    log_max_mb: Option<u64>,
    /// Rotated log files kept (default 5, or `log_files` of the configuration; 0: the file starts
    /// over when it is full).
    #[arg(long, global = true, value_name = "N")]
    log_files: Option<usize>,
}

/// The command line of a command such as `kobd setup` or `kobd db migrate`.
#[derive(Parser)]
#[command(name = "kobd", version = crate::version::long(), about = ABOUT, override_usage = USAGE, after_help = TOP_HELP)]
struct Cli {
    #[command(flatten)]
    log: LogArgs,
    #[command(subcommand)]
    cmd: Cmd,
}

/// The command line of a start of the Executor, or of both roles, with every option of the role.
#[derive(Parser)]
#[command(
    name = "kobd",
    version = crate::version::long(),
    about = "kobd --executor: the indexer, the matcher and the keepers in one process over one store (with --indexer too: the full history)",
    override_usage = "kobd --executor [--indexer] [OPTIONS]",
    after_help = START_HELP,
    args_override_self = true
)]
struct StartCli {
    /// The roles to start: `--executor`, `--indexer` or both (one process; the public API and the
    /// signing key then live on the same machine). Without a flag the configuration's roles start.
    #[command(flatten)]
    roles: RoleFlags,
    #[command(flatten)]
    run: RunArgs,
    #[command(flatten)]
    log: LogArgs,
}

/// The command line of a start of the Indexer alone, with the options of the indexer only.
#[derive(Parser)]
#[command(
    name = "kobd",
    version = crate::version::long(),
    about = "kobd --indexer: follow the chain, keep the full history and the record log, serve the read API (no key)",
    override_usage = "kobd --indexer [OPTIONS]",
    after_help = INDEX_HELP,
    args_override_self = true
)]
struct StartIndexCli {
    #[command(flatten)]
    roles: RoleFlags,
    #[command(flatten)]
    opts: IndexOpts,
    #[command(flatten)]
    log: LogArgs,
}

#[derive(Subcommand)]
enum Cmd {
    /// First run: choose the roles (Executor, Indexer, or both), check the node, make the operator
    /// key, wait for funds, write kobd.toml next to the binary, start.
    Setup(SetupArgs),
    /// The configuration: `kobd config check` reads kobd.toml offline and reports what a start
    /// would use.
    Config(ConfigArgs),
    /// The operator's address and its plain balance from the configured node, as one JSON object
    /// (never the secret).
    AddressBalance(AddressBalanceArgs),
    /// Send KAS out of the operator key over the configured node (the public resolver pair
    /// included). Stop the executor first.
    Withdraw(WithdrawArgs),
    /// The database of a data directory: info, migrate (bring an earlier build's store to this
    /// build's layout), vacuum (give its free pages back), replay (rebuild it from the record log),
    /// export-orders, export-book, import-orders, rebase.
    Db(DbArgs),
    /// Token-logo reports (`[logo_reports]`): review what was reported, restore a logo, hide one
    /// for good. Works next to a running executor of the same data directory (a short write to its
    /// database); there is no administrative route on the network.
    LogoReports(LogoReportsArgs),
    /// Batch-match crossing orders (spread + tips), trigger and arm stops in the batches, chain
    /// oversize crossings, from a book snapshot file.
    Match(MatchArgs),
    /// Refund / kill / close expired orders (paid by the orders' tips), sweep own strays, from a
    /// book snapshot file.
    Keep(KeepArgs),
    /// The x402 facilitator alone: GET /supported, POST /verify, POST /settle, GET /health, GET
    /// /metrics.
    X402(X402Args),
    /// Whether a running kobd is healthy: GET /v1/health of its read API (exit 0 when it answers
    /// 200 with "ok": true, else 1): a container's HEALTHCHECK, a monitoring probe.
    Health(HealthArgs),
}

/// `kobd health`.
#[derive(clap::Args)]
struct HealthArgs {
    /// The health route to ask. Default: /v1/health at the read API's address of kobd.toml (`[api]
    /// listen`; an unspecified address is asked on loopback), else http://127.0.0.1:8090/v1/health.
    #[arg(long)]
    url: Option<String>,
    /// Directory of kobd.toml. Default: $KOBD_DIR or the executable's own directory.
    #[arg(long)]
    dir: Option<PathBuf>,
    /// How long the answer may take, in seconds.
    #[arg(long, default_value_t = 5)]
    timeout_secs: u64,
}

/// `kobd config`.
#[derive(clap::Args)]
struct ConfigArgs {
    #[command(subcommand)]
    cmd: ConfigCmd,
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Check a configuration offline, without starting: parse it (an unknown or misspelt key is
    /// refused), resolve its paths and report the roles, the node, the data directory and its
    /// store, the read API, the log file, the pause file and the key file's access. Contacts no
    /// node, takes no lock, writes nothing. Exit status 1 when a start would refuse it, 2 when the
    /// file is missing or does not parse.
    Check {
        /// The configuration file. Default: kobd.toml of the executable's directory ($KOBD_DIR when
        /// set).
        #[arg(long, env = "KOB_INDEX_CONFIG")]
        config: Option<PathBuf>,
    },
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
    // A double click gives no arguments and a console that a person reads.
    let double_click = argv.len() == 1 && std::io::stdin().is_terminal();
    match start(argv) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Not `eprintln!`. A closed stderr would turn the exit status into a panic's (101).
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "Error: {e:?}");
            if double_click {
                // The console window closes with the process, so keep the error on screen until it
                // is read.
                let _ = writeln!(std::io::stderr(), "\nPress Enter to close this window.");
                let _ = setup::read_line();
            }
            // A configuration file that is missing or does not parse is a mistake that a restart
            // does not cure, so it exits like a usage error.
            let config = matches!(
                e.downcast_ref::<crate::config::ConfigError>(),
                Some(crate::config::ConfigError::Parse(..) | crate::config::ConfigError::NotFound(_))
            );
            if e.downcast_ref::<Usage>().is_some() || config {
                ExitCode::from(2)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn start(argv: Vec<OsString>) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let res = start_on(&rt, argv);
    // Dropping the runtime waits for every blocking call still running, such as a database read
    // or a lookup at a resolver. The timeout bounds that wait.
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

/// Whether the command line names a command (`kobd setup ...`) rather than a start (options only,
/// or nothing).
fn names_a_command(args: &[OsString]) -> bool {
    without_log_options(args).first().is_some_and(|a| !a.to_string_lossy().starts_with('-'))
}

/// Whether a start names nothing but roles (and log options): `kobd`, `kobd --executor`, ...
fn only_roles(args: &[OsString]) -> bool {
    without_log_options(args).iter().all(|a| matches!(a.to_string_lossy().as_ref(), "--executor" | "--indexer"))
}

/// The help page a start asks for (`-h`, `--help`): the top-level page without a role flag, the
/// Indexer's options for `--indexer` alone, the Executor's otherwise. `None`: no help asked.
#[derive(Debug, PartialEq, Eq)]
enum StartHelp {
    Top { long: bool },
    Indexer,
    Executor,
}

fn start_help(args: &[OsString]) -> Option<StartHelp> {
    let words: Vec<String> = without_log_options(args).iter().map(|w| w.to_string_lossy().into_owned()).collect();
    let long = words.iter().any(|w| w == "--help");
    if !long && !words.iter().any(|w| w == "-h") {
        return None;
    }
    let has = |f: &str| words.iter().any(|w| w == f);
    Some(match (has("--executor"), has("--indexer")) {
        (false, false) => StartHelp::Top { long },
        (false, true) => StartHelp::Indexer,
        _ => StartHelp::Executor,
    })
}

fn start_on(rt: &tokio::runtime::Runtime, argv: Vec<OsString>) -> anyhow::Result<()> {
    let program = argv.first().cloned().unwrap_or_else(|| "kobd".into());
    let args: Vec<OsString> = argv.iter().skip(1).cloned().collect();
    if names_a_command(&args) {
        return command(rt, program, Cli::parse_from(argv));
    }
    match start_help(&args) {
        Some(StartHelp::Top { long }) => {
            let mut c = Cli::command();
            if long {
                c.print_long_help()?;
            } else {
                c.print_help()?;
            }
            return Ok(());
        }
        Some(StartHelp::Indexer) => {
            // Prints the page of the Indexer and exits.
            StartIndexCli::parse_from(&argv);
        }
        _ => {}
    }
    // The options are checked, and --help and --version answered, before anything else happens.
    let cli = StartCli::parse_from(&argv);
    cli.run.index.node.refuse_old_node()?;
    logging::init(false);
    start_roles(rt, program, &args, &cli)
}

/// Runs a command.
fn command(rt: &tokio::runtime::Runtime, program: OsString, cli: Cli) -> anyhow::Result<()> {
    // A command that prints a document or a report on stdout keeps its log on stderr, so that
    // `kobd db export-orders | jq` works. The interactive setup, which starts the roles after
    // it, the facilitator, and the matcher and keepers that run keep the log on stdout.
    let log_to_stderr = match &cli.cmd {
        Cmd::Setup(a) => a.non_interactive,
        Cmd::Match(a) => a.common.offline,
        Cmd::Keep(a) => a.common.offline,
        Cmd::X402(_) => false,
        Cmd::Config(_) | Cmd::AddressBalance(_) | Cmd::Withdraw(_) | Cmd::Db(_) | Cmd::LogoReports(_) | Cmd::Health(_) => true,
    };
    logging::init(log_to_stderr);
    // The one-object JSON commands and the offline, read-only checks write no log file.
    let no_file = matches!(
        &cli.cmd,
        Cmd::AddressBalance(_)
            | Cmd::Config(_)
            | Cmd::Health(_)
            | Cmd::Setup(SetupArgs { non_interactive: true, .. })
            | Cmd::Db(DbArgs { sub: crate::index_cli::DbSub::Info { .. }, .. })
    );
    if !no_file {
        let config = match &cli.cmd {
            Cmd::Db(a) => a.store.config.as_deref(),
            Cmd::LogoReports(a) => a.store.config.as_deref(),
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
        Cmd::Config(ConfigArgs { cmd: ConfigCmd::Check { config } }) => config_check(config),
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
        Cmd::Health(a) => health_cmd(&a),
    }
}

/// The URL `kobd health` asks: `--url`, else /v1/health at `listen` of the read API (an unspecified
/// address on loopback).
fn health_url(url: Option<&str>, listen: Option<std::net::SocketAddr>) -> String {
    if let Some(u) = url {
        return u.to_string();
    }
    let mut a = listen.unwrap_or_else(|| std::net::SocketAddr::from(([127, 0, 0, 1], 8090)));
    if a.ip().is_unspecified() {
        a.set_ip(if a.is_ipv6() { std::net::Ipv6Addr::LOCALHOST.into() } else { std::net::Ipv4Addr::LOCALHOST.into() });
    }
    format!("http://{a}/v1/health")
}

/// Whether a `/v1/health` answer says healthy (`Ok`: its state) or why not.
fn health_verdict(status: u16, body: &str) -> Result<String, String> {
    if status != 200 {
        return Err(format!("HTTP {status}"));
    }
    let v: serde_json::Value = serde_json::from_str(body).map_err(|_| "the answer is not the health document".to_string())?;
    let state = v.get("state").and_then(serde_json::Value::as_str).unwrap_or("?").to_string();
    match v.get("ok").and_then(serde_json::Value::as_bool) {
        Some(true) => Ok(state),
        Some(false) => {
            let why = ["halt_reason", "gap_reason", "last_error"]
                .iter()
                .find_map(|k| v.get(*k).and_then(serde_json::Value::as_str))
                .map(|w| format!(": {w}"))
                .unwrap_or_default();
            Err(format!("not ok (state {state}{why})"))
        }
        None => Err("the answer has no \"ok\"".into()),
    }
}

/// `kobd health`: GET the read API's health route; exit 0 when it answers 200 with `"ok": true`,
/// else an error (exit 1).
fn health_cmd(a: &HealthArgs) -> anyhow::Result<()> {
    let listen = if a.url.is_some() {
        None
    } else {
        let dir = match &a.dir {
            Some(d) => d.clone(),
            None => setup::home_dir()?,
        };
        let path = dir.join(setup::CONFIG_FILE);
        match path.is_file().then(|| crate::config::IndexerConfig::load(&path)).transpose()? {
            Some(c) if !c.api.enabled => bail!("the read API is off in {}: nothing to ask (pass --url)", path.display()),
            Some(c) => Some(c.api.listen),
            None => None,
        }
    };
    let url = health_url(a.url.as_deref(), listen);
    let (status, body) =
        crate::rpc::tls::http_get(&url, std::time::Duration::from_secs(a.timeout_secs.max(1))).map_err(|e| anyhow!("{e}"))?;
    match health_verdict(status, &body) {
        Ok(state) => {
            println!("healthy: {url} ({state})");
            Ok(())
        }
        Err(why) => bail!("unhealthy: {url}: {why}"),
    }
}

/// A start: the configuration (`--config`, else `kobd.toml` of the folder) or the options alone.
/// `args` are the words of the command line without the program name, `cli` their parse.
fn start_roles(rt: &tokio::runtime::Runtime, program: OsString, args: &[OsString], cli: &StartCli) -> anyhow::Result<()> {
    let want = cli.roles.role();
    let path = match cli.run.index.config() {
        Some(p) => p.to_path_buf(),
        None => {
            let dir = setup::home_dir()?;
            let p = dir.join(setup::CONFIG_FILE);
            if !p.is_file() {
                if !only_roles(args) {
                    // Options and no configuration: the named roles start from the options alone.
                    let want = want.ok_or_else(|| {
                        usage(format!(
                            "no {} in {}: name the roles to start from the options alone (`kobd --executor ...`, `kobd --indexer \
                             ...`), or run `kobd setup`",
                            setup::CONFIG_FILE,
                            dir.display()
                        ))
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
                return Err(usage(format!(
                    "{} is set up for the {} but the command line asks for the {}: start it without role flags, or run `kobd setup` to change the roles",
                    path.display(),
                    have.display(),
                    want.display()
                )));
            }
            launch(rt, program, &path, args)
        }
        None => {
            // A plain --config file: the roles come from the command line.
            let want = want.ok_or_else(|| {
                usage(format!(
                    "{} has no [launch] table, so it names no roles: start `kobd --executor --config ...` or `kobd --indexer --config \
                     ...`, or run `kobd setup`",
                    path.display()
                ))
            })?;
            let start = parse_start(want, &program, args)?;
            run_start(rt, start)
        }
    }
}

/// The start of `roles` with the words `args` (without the program name): the Executor's options,
/// or the indexer's for the Indexer alone. The roles settle the storage: the Executor alone is
/// lean, with the Indexer too it keeps the full store. A mistake in the words is a [`Usage`] error.
fn parse_start(roles: AppRole, program: &OsString, args: &[OsString]) -> anyhow::Result<(Start, LogArgs)> {
    let argv = std::iter::once(program.clone()).chain(args.iter().cloned());
    match roles {
        AppRole::Indexer => {
            let c = StartIndexCli::try_parse_from(argv).map_err(|e| {
                usage(format!("the Indexer takes the indexer's options only (`kobd --indexer --help`): {}", e.render()))
            })?;
            c.opts.node.refuse_old_node()?;
            if c.opts.lean {
                return Err(usage("--lean: the Indexer keeps the full history; lean storage is the Executor's (`kobd --executor`)"));
            }
            Ok((Start::Index(Box::new(c.opts)), c.log))
        }
        roles => {
            let mut c = StartCli::try_parse_from(argv).map_err(|e| usage(e.render().to_string()))?;
            c.run.index.node.refuse_old_node()?;
            let o = &mut c.run.index;
            // Fast start follows the role, as lean storage does and as the kobd.toml of the setup
            // writes it. It is on for every start of the Executor, alone or with the Indexer,
            // and from options alone too. --no-fast-start or a `fast_start` key in the
            // configuration says otherwise. It does nothing on a data directory that has a
            // store already.
            if !o.fast_start && !o.no_fast_start && !config_sets_fast_start(o.config()) {
                o.fast_start = true;
            }
            if roles == AppRole::Executor {
                if o.no_lean {
                    return Err(usage(
                        "--no-lean: the Executor alone keeps lean storage; a full store with the history is `kobd --executor --indexer`",
                    ));
                }
                (o.lean, o.no_lean) = (true, false);
            } else {
                if o.lean {
                    return Err(usage(
                        "--lean: the Executor and Indexer keeps the full store; lean storage is the Executor alone (`kobd --executor`)",
                    ));
                }
                (o.lean, o.no_lean) = (false, true);
            }
            Ok((Start::Run(Box::new(c.run)), c.log))
        }
    }
}

/// Whether the configuration file at `path` has a `fast_start` key (either value): then the file
/// decides (`build_config` reads it), not the role. A file that cannot be read says nothing here
/// (the start reports it).
fn config_sets_fast_start(path: Option<&Path>) -> bool {
    path.and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| t.parse::<toml::Table>().ok())
        .is_some_and(|t| t.contains_key("fast_start"))
}

/// Runs a start that is not a launch configuration's.
///
/// There is no banner, but the log says whether trading waits for the indexer to catch up and
/// whether it is paused. A service started from options alone would otherwise give no hint why it
/// plans nothing for hours.
fn run_start(rt: &tokio::runtime::Runtime, (start, log): (Start, LogArgs)) -> anyhow::Result<()> {
    refuse_second_writer(&start)?;
    attach_log(&log, start.opts().config())?;
    if let Start::Run(a) = &start {
        let note = start_note(a);
        if !note.is_empty() {
            tracing::info!("{note}");
        }
        if let Some(p) = a.pause_file.as_deref().filter(|p| crate::pause::is_set(p)) {
            tracing::warn!(pause_file = %p.display(), "PAUSED at the start: the pause file exists (delete it to resume)");
        }
    }
    rt.block_on(dispatch(start))
}

/// Runs `setup --non-interactive`: writes the files and prints one JSON object on stdout.
fn setup_json(dir: std::path::PathBuf, a: &SetupArgs) -> anyhow::Result<()> {
    let Some(node) = a.node.clone() else {
        return Err(usage("--non-interactive needs --node resolver|<ws-url> and a role: --executor, --indexer or both"));
    };
    let Some(role) = a.roles.role() else {
        return Err(usage("--non-interactive needs a role: --executor, --indexer or both"));
    };
    let opts = SetupOptions::for_build(dir);
    let ask = AppSetup { node, role, new_key: a.new_key, network: a.network.clone() };
    println!("{}", app::setup_non_interactive(&opts, &ask)?);
    Ok(())
}

/// `withdraw`: the withdrawal over the configured node, the confirmation on the terminal. With
/// `--json` the result (or the refusal, `{"error": ...}`) is one JSON object on stdout and the
/// details go to stderr.
fn withdraw_cmd(rt: &tokio::runtime::Runtime, a: &WithdrawArgs) -> anyhow::Result<()> {
    let run = || -> anyhow::Result<serde_json::Value> {
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
        let mut node = withdraw::WrpcWithdrawNode::new(rt);
        if a.json {
            withdraw::withdraw(&cfg, &base, &req, &mut node, &mut setup::StderrConsole)
        } else {
            withdraw::withdraw(&cfg, &base, &req, &mut node, &mut StdConsole)
        }
    };
    match (run(), a.json) {
        (Ok(v), true) => {
            println!("{v}");
            Ok(())
        }
        (Err(e), true) => {
            println!("{}", serde_json::json!({ "error": format!("{e:#}") }));
            Err(e)
        }
        (r, false) => r.map(|_| ()),
    }
}

/// Runs the interactive setup into `dir`, then, unless declined, the configuration it wrote.
fn setup_then_launch(
    rt: &tokio::runtime::Runtime,
    program: OsString,
    dir: std::path::PathBuf,
    no_start: bool,
    roles: Option<AppRole>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&dir)?;
    // Keep the dialogue clean. The log lines of the node checks go to the log file only, the one
    // that a run appends to. The console shows the log again for what follows.
    let console_was = logging::console_shows_log();
    if logging::attach_file(LogOptions::new(dir.join(setup::LOG_FILE), 10, 5)).is_ok() {
        logging::set_console(false);
    }
    let opts = SetupOptions::for_build(dir);
    let done = setup::run_setup_with(&mut StdConsole, &mut WrpcSetupNode { rt }, &opts, roles, no_start);
    logging::set_console(console_was);
    match done? {
        Some(cfg) => {
            logging::console_line("", false);
            launch(rt, program, &cfg, &[])
        }
        _ => Ok(()),
    }
}

/// The words of a command line without `--config` and its value (a launch names its file itself)
/// and without the role flags (the launch names the file's, which the command line's were checked
/// against).
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

/// Runs the configuration at `cfg` (`setup::launch_args`) in the file's directory, with the options
/// `args` of the command line after the file's (a later option wins).
fn launch(rt: &tokio::runtime::Runtime, program: OsString, cfg: &Path, args: &[OsString]) -> anyhow::Result<()> {
    let l = setup::launch_args(cfg)?;
    // Read while the working directory is still the caller's, because `cfg` may be a relative path.
    let address = app::operator_address(cfg);
    // Check the options and the data directory first. A second instance is refused before it
    // prints a banner or touches the log.
    std::env::set_current_dir(&l.dir).map_err(|e| anyhow!("cannot change to {}: {e}", l.dir.display()))?;
    let mut words: Vec<OsString> = l.args.iter().map(OsString::from).collect();
    words.extend(without_config(args));
    let (start, log) = parse_start(l.roles, &program, &words).map_err(|e| {
        let text = format!("the start options of {} ([launch] and the command line): {e}", cfg.display());
        if e.downcast_ref::<Usage>().is_some() {
            usage(text)
        } else {
            anyhow!(text)
        }
    })?;
    refuse_second_writer(&start)?;
    // The banner goes to the console as the log does. A closed console never takes the start down.
    let say = |line: &str| logging::console_line(line, false);
    say(&format!("Roles: {}.", l.roles.display()));
    say(&format!("Running {} (the same as):", cfg.display()));
    let shown = setup::Launch { args: words.iter().map(|w| w.to_string_lossy().into_owned()).collect(), ..l.clone() };
    for line in setup::command_line(&shown, &setup::program_path()).lines() {
        say(&format!("    {line}"));
    }
    let mut paused = None;
    match &start {
        Start::Run(a) => {
            say(&start_note(a));
            if let Some(p) = &a.pause_file {
                let p = l.dir.join(p);
                if crate::pause::is_set(&p) {
                    say(&format!("PAUSED: {} exists, so nothing is built or submitted; delete it to resume.", p.display()));
                    paused = Some(p);
                } else {
                    say(&format!("Pause trading without stopping: create the file {} (delete it to resume).", p.display()));
                }
            }
        }
        Start::Index(_) => say("The Indexer follows the chain and serves the read API; it has no key."),
    }
    if let Some(a) = address {
        say(&format!("Funding address (send KAS here; `{} address-balance` shows it and its balance): {a}", setup::program_path()));
    }
    if let Ok(c) = crate::index_cli::build_config(start.opts()) {
        if c.api.enabled {
            say(&format!("Read API: http://{}/v1/health", c.api.listen));
        }
    }
    attach_log(&log, start.opts().config())?;
    match start.opts().status_every() {
        Some(every) => say(&format!("Stop it with Ctrl+C. A status line follows every {} s.", every.as_secs())),
        None => say("Stop it with Ctrl+C."),
    }
    say("");
    if let Some(p) = paused {
        tracing::warn!(pause_file = %p.display(), "PAUSED at the start: the pause file exists (delete it to resume)");
    }
    rt.block_on(dispatch(start))
}

/// Refuses a start that writes a data directory another kobd process holds, before anything is
/// printed or written to the log file (the running process has it open, and it may be the same
/// file).
fn refuse_second_writer(start: &Start) -> anyhow::Result<()> {
    // A configuration that does not build is reported by the start itself.
    if let Ok(cfg) = crate::index_cli::build_config(start.opts()) {
        crate::indexer::lock::WriterLock::check(&cfg)?;
    }
    Ok(())
}

/// What the banner says about the start of the Executor: whether trading waits for the history (it
/// does not with the fast start).
fn start_note(a: &RunArgs) -> String {
    use crate::faststart::Plan;
    let Ok(cfg) = crate::index_cli::build_config(&a.index) else { return String::new() };
    // The book of the facilitator is bound to one store, so there is no fast start with it. A dry
    // run serves no facilitator.
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

/// The log file of the command line (`--log-file`) or of the configuration `cfg_path` (`log_file`,
/// relative to the configuration's directory), attached to the log with the rotation settings: the
/// path and the rotation. `None`: no file.
fn log_file_of(log: &LogArgs, cfg_path: Option<&Path>) -> Option<LogOptions> {
    let cfg = cfg_path.and_then(|p| crate::config::IndexerConfig::load(p).ok());
    let base = cfg_path.and_then(|p| p.parent()).filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let file = match (&log.log_file, cfg.as_ref().and_then(|c| c.log_file.as_ref())) {
        (Some(f), _) => f.clone(),
        (None, Some(f)) => logging::resolve(base, f),
        (None, None) => return None,
    };
    let max_mb = log.log_max_mb.or(cfg.as_ref().map(|c| c.log_max_mb)).unwrap_or(10);
    let keep = log.log_files.or(cfg.as_ref().map(|c| c.log_files)).unwrap_or(5);
    Some(LogOptions::new(file, max_mb, keep))
}

/// How the banner names a log file: `<path> (a new file at N MB, K old ones kept)`.
fn log_file_text(o: &LogOptions) -> String {
    let abs = std::path::absolute(&o.file).unwrap_or_else(|_| o.file.clone());
    format!("{} (a new file at {} MB, {} old ones kept)", abs.display(), o.max_bytes >> 20, o.keep)
}

/// Attaches the log file ([`log_file_of`]) and says where it is, always: the console shows the log
/// too, and the file is where a scheduled task's or a service's history is read.
fn attach_log(log: &LogArgs, cfg_path: Option<&Path>) -> anyhow::Result<()> {
    let Some(o) = log_file_of(log, cfg_path) else { return Ok(()) };
    let line = format!("Log file: {}", log_file_text(&o));
    logging::attach_file(o.clone()).map_err(|e| anyhow!("cannot open the log file {}: {e}", o.file.display()))?;
    logging::console_line(&line, logging::on_stderr());
    Ok(())
}

async fn dispatch(start: Start) -> anyhow::Result<()> {
    match start {
        Start::Run(a) => executor::run(*a).await,
        Start::Index(o) => crate::index_cli::follow_and_serve(*o).await,
    }
}

/// `kobd config check`: what a start of the configuration at `path` would use, read offline
/// ([`check_lines`]); an error when a start would refuse it.
fn config_check(path: Option<PathBuf>) -> anyhow::Result<()> {
    let path = match path {
        Some(p) => p,
        None => setup::home_dir()?.join(setup::CONFIG_FILE),
    };
    let (lines, problems) = check_lines(&path)?;
    for l in &lines {
        println!("{l}");
    }
    if problems.is_empty() {
        println!("OK: a start would use this configuration as shown (checked offline: no node was contacted, nothing was written).");
        return Ok(());
    }
    for p in &problems {
        println!("PROBLEM: {p}");
    }
    bail!("{} problem(s) in {}: a start would refuse it or misbehave", problems.len(), path.display())
}

/// The report of `kobd config check` on the file at `path` (absolute paths), and the problems a
/// start would refuse. The file must parse (an error otherwise). Nothing is written and no node is
/// contacted; the key file is read to check its access and its form, its secret is never shown (the
/// funding address is).
fn check_lines(path: &Path) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    use crate::config::IndexerConfig;
    let abs = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let cfg_file = abs(path);
    let parsed = IndexerConfig::load(&cfg_file)?;
    let dir = cfg_file.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let mut lines = vec![format!("Configuration: {} (parsed: every key known)", cfg_file.display())];
    let mut problems = vec![];
    // The start that the file stands for: its [launch] options in its directory, as `kobd` would
    // run them. The paths of the options are relative to the directory of the file, because the
    // launcher changes to it first.
    let program = OsString::from("kobd");
    let start = match AppRole::of_config(&parsed) {
        Some(roles) => {
            lines.push(format!("Roles: {} ([launch] role)", roles.display()));
            match setup::launch_args(&cfg_file) {
                Ok(l) => {
                    let mut words: Vec<OsString> = vec![];
                    let mut it = l.args.iter();
                    while let Some(w) = it.next() {
                        words.push(w.into());
                        // The values of the path options, against the directory of the file.
                        if matches!(w.as_str(), "--config" | "--key-file" | "--pause-file" | "--x402-config") {
                            if let Some(v) = it.next() {
                                words.push(logging::resolve(&dir, Path::new(v)).into_os_string());
                            }
                        }
                    }
                    match parse_start(roles, &program, &words) {
                        Ok((s, _)) => Some(s),
                        Err(e) => {
                            problems.push(format!("the start options ([launch] args): {e}"));
                            None
                        }
                    }
                }
                Err(e) => {
                    problems.push(format!("{e}"));
                    None
                }
            }
        }
        None => {
            lines.push("Roles: none named (no [launch] table): the command line names them, `kobd --executor --config ...`".into());
            let mut o = IndexOpts::default();
            o.store.config = Some(cfg_file.clone());
            Some(Start::Index(Box::new(o)))
        }
    };
    let Some(start) = start else { return Ok((lines, problems)) };
    let cfg = match crate::index_cli::build_config(start.opts()) {
        Ok(c) => c,
        Err(e) => {
            problems.push(format!("{e:#}"));
            return Ok((lines, problems));
        }
    };
    lines.push(format!("Network: {} ({})", cfg.network, crate::version::build_kind()));
    let primary = cfg.primary_url();
    lines.push(if crate::rpc::resolver::is_resolver(primary) {
        "Node: resolver (two public nodes found through the Kaspa resolvers, one cross-checking the other)".to_string()
    } else {
        format!("Node: {} (the chain authority)", crate::rpc::redact_url(primary))
    });
    let extra: Vec<String> = cfg
        .secondaries()
        .iter()
        .filter(|n| !(crate::rpc::resolver::is_resolver(primary) && n.url == crate::rpc::resolver::pseudo(2)))
        .map(|n| crate::rpc::redact_url(&n.url))
        .collect();
    if !extra.is_empty() {
        lines.push(format!("Extra nodes: {}", extra.join(", ")));
    }
    if cfg.failover.enabled() {
        lines.push(format!("Failover: to {} when the node stops answering", cfg.failover.fallback));
    }
    match crate::index_cli::store_info(&cfg) {
        Ok(v) => lines.extend(crate::index_cli::store_info_lines(&v)),
        Err(e) => problems.push(format!("the store: {e:#}")),
    }
    lines.push(if cfg.api.enabled { format!("Read API: http://{}/v1/health", cfg.api.listen) } else { "Read API: off".into() });
    let log = LogArgs::default();
    lines.push(match log_file_of(&log, Some(&cfg_file)) {
        Some(o) => format!("Log file: {}", log_file_text(&o)),
        None => "Log file: none (the console only)".into(),
    });
    match &start {
        Start::Run(a) => {
            match &a.pause_file {
                Some(p) if crate::pause::is_set(p) => {
                    lines.push(format!("Pause file: {} EXISTS: the executor starts paused", p.display()))
                }
                Some(p) => lines.push(format!("Pause file: {} (absent: create it to pause)", p.display())),
                None => lines.push("Pause file: none (no way to pause without stopping)".into()),
            }
            if let Err(e) = executor::refuse_files_in_data_dir(a.key_file.as_deref(), a.pause_file.as_deref(), &cfg.data_dir) {
                problems.push(e.to_string());
            }
            match &a.key_file {
                Some(k) => match crate::matcher::wallet::HotKey::from_file(k) {
                    Ok(key) => lines.push(format!(
                        "Key file: {} (readable by this account only; funding address {})",
                        k.display(),
                        key.address(&cfg.network)
                    )),
                    Err(e) => problems.push(e.to_string()),
                },
                None => {
                    lines.push("Key file: none in the configuration (KOB_OPERATOR_KEY_FILE or a systemd credential gives it)".into())
                }
            }
            if let Some(x) = a.x402_config.as_deref() {
                lines.push(format!("x402 facilitator: {}{}", x.display(), if x.is_file() { "" } else { " (MISSING)" }));
                if !x.is_file() {
                    problems.push(format!("the x402 configuration {} does not exist", x.display()));
                }
            }
            if let Err(e) = executor::refuse_failover(&cfg) {
                problems.push(e.to_string());
            }
        }
        Start::Index(o) => {
            if AppRole::of_config(&parsed) == Some(AppRole::Indexer) {
                if let Err(e) = crate::index_cli::refuse_run_only_options(&cfg, o.config()) {
                    problems.push(e.to_string());
                }
            }
        }
    }
    Ok((lines, problems))
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
        // Fast start follows the role too, as `kobd setup` writes it, from options alone.
        assert_eq!(run(AppRole::Executor, &["--executor", "--dry-run"]), Ok((true, false, true, true)));
        assert_eq!(run(AppRole::Executor, &["--lean"]), Ok((true, false, false, true)));
        assert!(run(AppRole::Executor, &["--no-lean"]).unwrap_err().contains("kobd --executor --indexer"));
        assert_eq!(run(AppRole::ExecutorIndexer, &[]), Ok((false, true, false, true)));
        assert_eq!(run(AppRole::Executor, &["--no-fast-start"]), Ok((true, false, false, false)), "--no-fast-start turns it off");
        // A `fast_start` key in the configuration decides, either way. Without the key the role's
        // default applies.
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
        // A later option wins, so the command line overrides the [launch] options.
        let Ok((Start::Run(a), _)) =
            parse_start(AppRole::Executor, &p, &words(&["--listen", "127.0.0.1:1", "--listen", "127.0.0.1:2"]))
        else {
            panic!("a start")
        };
        assert_eq!(a.index.listen, Some("127.0.0.1:2".parse().unwrap()));
        // The Indexer takes no option of the Executor and no lean storage.
        assert!(matches!(
            parse_start(AppRole::Indexer, &p, &words(&["--indexer", "--listen", "127.0.0.1:1"])),
            Ok((Start::Index(_), _))
        ));
        let e = parse_start(AppRole::Indexer, &p, &words(&["--dry-run"])).err().unwrap();
        assert!(e.downcast_ref::<Usage>().is_some(), "a usage mistake: exit status 2");
        let e = e.to_string();
        assert!(
            e.contains("the Indexer takes the indexer's options only (`kobd --indexer --help`)") && e.contains("--dry-run"),
            "{e}"
        );
        assert!(parse_start(AppRole::Indexer, &p, &words(&["--lean"])).is_err());
    }

    /// `--node` is refused on every role and names the options that replace it. It is never taken
    /// as the node or as a further node. `--extra-node` is the further node.
    #[test]
    fn the_old_node_option_is_a_usage_error_with_the_new_names() {
        let p = OsString::from("kobd");
        for role in [AppRole::Executor, AppRole::Indexer, AppRole::ExecutorIndexer] {
            let e = parse_start(role, &p, &words(&["--node", "ws://127.0.0.1:18210"])).err().expect("refused");
            assert!(e.downcast_ref::<Usage>().is_some(), "{role:?}");
            assert!(e.to_string().contains("--extra-node") && e.to_string().contains("--rpc-url"), "{e}");
        }
        let Ok((Start::Run(a), _)) = parse_start(AppRole::Executor, &p, &words(&["--extra-node", "ws://10.0.0.5:18210"])) else {
            panic!("a start")
        };
        assert_eq!(a.index.node.nodes, ["ws://10.0.0.5:18210"]);
    }

    #[test]
    fn help_asked_on_a_start_shows_the_page_of_its_roles() {
        assert_eq!(start_help(&words(&["--executor", "--dry-run"])), None);
        assert_eq!(start_help(&words(&["--help"])), Some(StartHelp::Top { long: true }));
        assert_eq!(start_help(&words(&["-h"])), Some(StartHelp::Top { long: false }));
        assert_eq!(start_help(&words(&["--indexer", "--help"])), Some(StartHelp::Indexer));
        assert_eq!(start_help(&words(&["--executor", "-h"])), Some(StartHelp::Executor));
        assert_eq!(start_help(&words(&["--executor", "--indexer", "--help"])), Some(StartHelp::Executor));
        assert_eq!(start_help(&words(&["--log-file", "--help"])), None, "a value of a log option is not a flag");
        // The pages build, which clap checks in debug builds, and name what they promise.
        let top = Cli::command().render_long_help().to_string();
        for w in ["kobd --executor --help", "kobd --indexer --help", "setup", "config", "withdraw", "db", "Exit status"] {
            assert!(top.contains(w), "{w}: {top}");
        }
        let run = StartCli::command().render_long_help().to_string();
        for w in ["Roles:", "Node:", "Executor:", "Fees:", "Matcher:", "Keepers:", "Log:", "--extra-node", "--key-file"] {
            assert!(run.contains(w), "{w}: {run}");
        }
        assert!(!run.contains("\n      --node "), "the old option is hidden: {run}");
        let index = StartIndexCli::command().render_long_help().to_string();
        assert!(
            index.contains("--listen")
                && !index.contains(
                    "
      --dry-run"
                )
                && !index.contains(
                    "
      --key-file"
                ),
            "{index}"
        );
    }

    /// `kobd health` asks the read API of kobd.toml, on loopback for an unspecified address. It is
    /// healthy only on 200 with `"ok": true`. This runs it against a local server end to end.
    #[test]
    fn health_asks_the_read_api_and_needs_ok() {
        use std::net::SocketAddr;
        assert_eq!(health_url(None, None), "http://127.0.0.1:8090/v1/health");
        assert_eq!(health_url(None, Some("0.0.0.0:9000".parse::<SocketAddr>().unwrap())), "http://127.0.0.1:9000/v1/health");
        assert_eq!(health_url(None, Some("[::]:9000".parse::<SocketAddr>().unwrap())), "http://[::1]:9000/v1/health");
        assert_eq!(health_url(Some("http://h:1/v1/health"), None), "http://h:1/v1/health");
        assert_eq!(health_verdict(200, r#"{"ok": true, "state": "following"}"#), Ok("following".to_string()));
        let e = health_verdict(200, r#"{"ok": false, "state": "gap", "gap_reason": "downtime exceeded"}"#).unwrap_err();
        assert!(e.contains("state gap") && e.contains("downtime exceeded"), "{e}");
        assert!(health_verdict(503, "").unwrap_err().contains("HTTP 503"));
        assert!(health_verdict(200, "<html>").is_err() && health_verdict(200, "{}").is_err());
        // End to end: a local server that answers the health document once.
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let srv = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut b = [0u8; 1024];
            let _ = c.read(&mut b);
            let body = r#"{"ok":true,"state":"following"}"#;
            let _ = c.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes());
        });
        let a = HealthArgs { url: Some(format!("http://127.0.0.1:{port}/v1/health")), dir: None, timeout_secs: 5 };
        health_cmd(&a).unwrap();
        srv.join().unwrap();
        // Nothing listens: an error, exit 1.
        let a = HealthArgs { url: Some(format!("http://127.0.0.1:{port}/v1/health")), dir: None, timeout_secs: 5 };
        assert!(health_cmd(&a).is_err());
    }

    /// `kobd config check` on a configuration the setup wrote: the roles, the node, the store, the
    /// files, offline.
    #[test]
    fn config_check_reports_a_setup_configuration_offline() {
        let _g = crate::rpc::resolver::tests::serial();
        let d = tempfile::tempdir().unwrap();
        let net = kob_protocol::artifacts::deployment_network().unwrap_or("testnet-10");
        let mut opts = SetupOptions::for_build(d.path().to_path_buf());
        opts.build_network = Some(net.to_string());
        let ask = AppSetup { node: "resolver".into(), role: AppRole::Executor, new_key: true, network: Some(net.into()) };
        app::setup_non_interactive(&opts, &ask).unwrap();
        let (lines, problems) = check_lines(&d.path().join(setup::CONFIG_FILE)).unwrap();
        let text = lines.join("\n");
        assert!(problems.is_empty(), "{problems:?}\n{text}");
        for w in [
            "Roles: Executor",
            "Node: resolver",
            "does not exist yet",
            "Pause file:",
            "Key file:",
            "funding address",
            "Read API: http://",
        ] {
            assert!(text.contains(w), "{w}: {text}");
        }
        let secret = std::fs::read_to_string(d.path().join(setup::KEY_FILE)).unwrap();
        assert!(!text.contains(secret.trim()), "never the secret");
        // A pause file is reported. A key file inside the data directory is a problem.
        std::fs::write(d.path().join(setup::PAUSE_FILE), "").unwrap();
        let (lines, _) = check_lines(&d.path().join(setup::CONFIG_FILE)).unwrap();
        assert!(lines.join("\n").contains("EXISTS: the executor starts paused"));
        // A misspelt key is refused with its name.
        std::fs::write(d.path().join("bad.toml"), "netwrok = \"testnet-10\"\n").unwrap();
        let e = check_lines(&d.path().join("bad.toml")).unwrap_err().to_string();
        assert!(e.contains("netwrok"), "{e}");
        // No file: the message says how to create one.
        let e = check_lines(&d.path().join("none.toml")).unwrap_err().to_string();
        assert!(e.contains("kobd setup"), "{e}");
    }
}
