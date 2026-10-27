//! First run for an operator who downloaded the binary: `kob-executor setup`, and the binary started without arguments
//! (what a double click does).
//!
//! Started without arguments, the binary looks for [`CONFIG_FILE`] in its own directory ([`no_args_start`]). With one, it
//! runs what the file's `[launch]` table says ([`launch_args`]): exactly `kob-executor <role> --config kob-executor.toml
//! ...` in the file's directory, printed before it starts, so the same line moves unchanged to a service. Without one, and
//! on a terminal, it runs the interactive setup ([`run_setup`]), which writes the file and then starts the same way.
//!
//! Nothing here talks to anything but the node the operator names (the local node of the build's network by default,
//! [`default_wrpc_url`]): no telemetry, no fee, no project server, no update check.
//!
//! The dialogue goes through [`Console`] (prompts and answers) and [`SetupNode`] (the two node calls), so it runs in tests
//! on scripted answers and a fake node; [`StdConsole`] and [`WrpcSetupNode`] are the real ones.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;

use crate::config::{default_wrpc_url, IndexerConfig, LaunchConfig, LaunchRole};
use crate::matcher::node::{NodeApi, ServerInfo, WrpcConfig, WrpcNode};
use crate::matcher::run::kas_text;
use crate::matcher::wallet::HotKey;

/// The configuration a start without arguments reads, next to the executable.
pub const CONFIG_FILE: &str = "kob-executor.toml";
/// The operator key file the setup generates, next to the configuration.
pub const KEY_FILE: &str = "kob-operator.key";
/// The log file the interactive setup configures, relative to the configuration.
pub const LOG_FILE: &str = "kob-executor.log";
/// The default data directory (database and record log), relative to the configuration.
pub const DATA_DIR: &str = "kob-data";
/// The token-program list a build without a pinned registry gets (testnet-10, [`program_registry`]).
pub const TOKENS_FILE: &str = "kob-tokens.json";
/// The balance at which the setup stops waiting for funds: `run`'s low-funds alert threshold (`--low-funds-kas`).
pub const MIN_FUNDS_KAS: u64 = 100;
/// The funding the setup recommends: twice the alert threshold, so a day of trading does not start with a warning.
pub const RECOMMENDED_FUNDS_KAS: u64 = 200;

/// `kob-executor setup`.
#[derive(clap::Args, Debug, Clone)]
pub struct SetupArgs {
    /// Directory of the configuration, the key file and the data directory. Default: the one a start without arguments
    /// reads, `$KOB_EXECUTOR_DIR` or else the executable's own directory.
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// Write the configuration and exit instead of starting the executor.
    #[arg(long)]
    pub no_start: bool,
    /// For a wrapper (the Android app) that cannot answer questions: no dialogue, no start. Needs `--node` and `--role`; writes
    /// the files and prints one JSON object (`address`, `config`, ...; never the secret). Replaces an existing configuration,
    /// keeps an existing key file.
    #[arg(long, requires_all = ["node", "role"])]
    pub non_interactive: bool,
    /// Non-interactive: `resolver` (two public nodes, the default of the interactive setup) or your node's wRPC URL.
    #[arg(long, requires = "non_interactive")]
    pub node: Option<String>,
    /// Non-interactive: `match` (matcher and keepers; needs a key) or `index` (index only, no key).
    #[arg(long, value_enum, requires = "non_interactive")]
    pub role: Option<crate::app::AppRole>,
    /// Non-interactive: make the operator key when the directory has none (an existing key file is never replaced).
    #[arg(long, requires = "non_interactive")]
    pub new_key: bool,
    /// Non-interactive, development builds only: `mainnet` or `testnet-10` (a deployment build knows its network).
    #[arg(long, requires = "non_interactive")]
    pub network: Option<String>,
}

/// What a start without arguments does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoArgs {
    /// Run the configuration at this path ([`launch_args`]).
    Launch(PathBuf),
    /// No configuration, and a person at a terminal: the interactive setup.
    Setup,
    /// No configuration and no terminal (a service, a pipe): nobody can answer the setup's questions.
    Refuse(String),
}

/// The decision of a start without arguments, for the executable's directory `dir`.
pub fn no_args_start(dir: &Path, interactive: bool) -> NoArgs {
    let cfg = dir.join(CONFIG_FILE);
    if cfg.is_file() {
        NoArgs::Launch(cfg)
    } else if interactive {
        NoArgs::Setup
    } else {
        NoArgs::Refuse(format!(
            "no {} in {} and no terminal to ask the setup's questions on: run `kob-executor setup` in a terminal, or give a \
             subcommand (`kob-executor --help`)",
            CONFIG_FILE,
            dir.display()
        ))
    }
}

/// Environment variable naming the directory a start without arguments and `setup` use instead of the executable's own (a
/// container's volume, where the executable's directory is part of the read-only image).
pub const DIR_ENV: &str = "KOB_EXECUTOR_DIR";

/// The directory of the configuration: `$KOB_EXECUTOR_DIR` when set, else the executable's directory.
pub fn home_dir() -> Result<PathBuf> {
    home_dir_from(std::env::var_os(DIR_ENV))
}

fn home_dir_from(env: Option<std::ffi::OsString>) -> Result<PathBuf> {
    match env.filter(|d| !d.is_empty()) {
        Some(d) => Ok(PathBuf::from(d)),
        None => exe_dir(),
    }
}

/// The directory of the running executable.
pub fn exe_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("cannot locate the executable")?;
    exe.parent().map(Path::to_path_buf).ok_or_else(|| anyhow!("the executable {} has no directory", exe.display()))
}

/// The running executable's full path, for a printed command line that works from any directory (`kob-executor` when it
/// cannot be named).
pub fn program_path() -> String {
    std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "kob-executor".into())
}

/// A start from a configuration file: run `args` (the subcommand and its options, without the program name) in `dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub dir: PathBuf,
    pub args: Vec<String>,
    pub role: LaunchRole,
    /// The read API's address (`None`: off), for the operator's first look.
    pub api: Option<std::net::SocketAddr>,
    pub pause_file: Option<PathBuf>,
}

/// The command line the configuration at `path` stands for. The file must parse as a `--config` file (every indexer key is
/// checked, unknown ones refused) and carry a `[launch]` table; the paths it names stay as written, relative to the file's
/// directory, which the caller makes the working directory.
pub fn launch_args(path: &Path) -> Result<Launch> {
    let cfg = IndexerConfig::load(path)?;
    let launch = cfg.launch.clone().ok_or_else(|| {
        anyhow!("{} has no [launch] table: run `kob-executor setup`, or start a subcommand with --config", path.display())
    })?;
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf();
    let name = path.file_name().ok_or_else(|| anyhow!("{} is not a file", path.display()))?;
    let mut args = vec![launch.role.subcommand().to_string(), "--config".to_string(), name.to_string_lossy().into_owned()];
    let run_only = [("key_file", &launch.key_file), ("pause_file", &launch.pause_file), ("x402_config", &launch.x402_config)];
    match launch.role {
        LaunchRole::Run => {
            for (flag, v) in run_only {
                if let Some(p) = v {
                    args.push(format!("--{}", flag.replace('_', "-")));
                    args.push(p.to_string_lossy().into_owned());
                }
            }
        }
        LaunchRole::Index => {
            if let Some(k) = launch.run_only_key() {
                bail!("{}: [launch] {k} is an option of role \"run\", not of role \"index\"", path.display());
            }
        }
    }
    args.extend(launch.args.iter().cloned());
    let api = cfg.api.enabled.then_some(cfg.api.listen);
    Ok(Launch { dir, args, role: launch.role, api, pause_file: launch.pause_file })
}

/// `s` as one shell word: as is when it needs no quoting, else in double quotes.
fn shell_word(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./\\:=@+,".contains(c)) {
        s.to_string()
    } else {
        format!("\"{}\"", s.replace('"', "\\\""))
    }
}

/// The two lines that run `launch` from a terminal (Windows `cmd`, or a POSIX shell): change to its directory, then the
/// program with its arguments. What a service runs (docs/ops/executor-quickstart.md).
pub fn command_line(launch: &Launch, program: &str) -> String {
    let cd = if cfg!(windows) { "cd /d" } else { "cd" };
    let args: Vec<String> = launch.args.iter().map(|a| shell_word(a)).collect();
    format!("{cd} {}\n{} {}", shell_word(&launch.dir.to_string_lossy()), shell_word(program), args.join(" "))
}

/// The configuration file the setup writes: the indexer keys it chose and the `[launch]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetupFile {
    pub network: String,
    pub rpc_url: String,
    pub data_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_path: Option<String>,
    /// `fast_start = true` for the matcher role: the first start trades within seconds and indexes the history meanwhile
    /// (`crate::faststart`). Absent for the index role, which has no use for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fast_start: Option<bool>,
    /// `lean = true` for the matcher role: the store keeps the live book, not the history (`crate::indexer::lean`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lean: Option<bool>,
    /// `log_file` for the matcher role: the console shows the log while it is a terminal (a double click), and a scheduled task or
    /// service, which has no console, finds it in the file (rotated by size, `crate::logging`). Absent for the index role and
    /// for the wrapper's non-interactive setup (the Android app reads the process's output).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_file: Option<String>,
    pub launch: LaunchConfig,
}

impl SetupFile {
    /// The `lean` key a setup writes for `role`.
    pub fn lean_for(role: LaunchRole) -> Option<bool> {
        (role == LaunchRole::Run).then_some(true)
    }

    /// The `log_file` key the interactive setup writes for `role`.
    pub fn log_file_for(role: LaunchRole) -> Option<String> {
        (role == LaunchRole::Run).then(|| LOG_FILE.to_string())
    }

    /// The `fast_start` key a setup writes for `role`.
    pub fn fast_start_for(role: LaunchRole) -> Option<bool> {
        (role == LaunchRole::Run).then_some(true)
    }
}

impl SetupFile {
    /// The file's text: a header that says what it is, then the keys (a valid `--config` file).
    pub fn text(&self) -> Result<String> {
        let body = toml::to_string(self).context("cannot write the configuration")?;
        Ok(format!(
            "# kob-executor configuration, written by `kob-executor setup`.\n\
             #\n\
             # Started without arguments, kob-executor reads this file from its own directory and runs the [launch] role with\n\
             # it (`kob-executor {} --config {CONFIG_FILE} ...`, printed at the start). Relative paths are relative to this\n\
             # file's directory. Every key of docs/ops/executor.md (Part B, 4) may be added; [launch] args takes further\n\
             # options of the role, e.g. args = [\"--sweep-own-strays\"]. `kob-executor setup` again replaces this file.\n\n{body}",
            self.launch.role.subcommand()
        ))
    }
}

/// The token list of a build that pins no registry (testnet-10): the programs of the embedded registry for `network`, with
/// no token entries (the mainnet entries name mainnet tokens). A registry document with templates makes the allowlist open:
/// every order of a supported program is listed (docs/ops/executor.md, *Token allowlist*), as the mainnet build lists with
/// its embedded registry. That is KOB's no-curation policy: no token is listed by name and none refused for what it is (a
/// bare list of chosen tokens would be a listing decision). Without any list nothing would be listed.
pub fn program_registry(network: &str) -> Result<String> {
    let mut v: serde_json::Value = serde_json::from_str(kob_protocol::registry::DEFAULT_REGISTRY_JSON)?;
    let o = v.as_object_mut().ok_or_else(|| anyhow!("the embedded registry is not an object"))?;
    o.remove("$schema");
    o.insert("network".into(), network.into());
    o.insert("tokens".into(), serde_json::Value::Array(vec![]));
    let text = serde_json::to_string_pretty(&v)? + "\n";
    let list = crate::tokens::TokenAllowlist::parse(&text).map_err(|e| anyhow!("the program list does not parse: {e}"))?;
    if !list.is_open() {
        bail!("the program list names no supported program");
    }
    Ok(text)
}

/// One answer while the setup waits for something else (the funds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    Line(String),
    Nothing,
    Closed,
}

/// The setup's terminal.
pub trait Console {
    /// Prints a line.
    fn say(&mut self, line: &str);
    /// Prints `prompt` and returns the next answer, trimmed (`None`: the input ended).
    fn ask(&mut self, prompt: &str) -> Option<String>;
    /// An answer typed within `wait`, if any.
    fn poll(&mut self, wait: Duration) -> Poll;
}

/// The two node calls of the setup.
pub trait SetupNode {
    /// `getServerInfo`.
    fn server_info(&mut self, url: &str) -> Result<ServerInfo, String>;
    /// The plain KAS (no covenant) the node's UTXO index holds for `address`, in sompi.
    fn balance(&mut self, url: &str, address: &str) -> Result<u64, String>;
    /// The host a resolver entry uses now, for the operator (`None`: not resolved).
    fn node_name(&mut self, url: &str) -> Option<String> {
        crate::rpc::resolver::current(url).map(|n| crate::rpc::resolver::host(&n))
    }
}

/// The lines of standard input, read on a thread of their own so that [`Console::poll`] can wait with a timeout. One reader
/// for the whole process: the "press Enter to close" prompt after a fatal error reads the same lines ([`read_line`]).
fn stdin_lines() -> &'static Mutex<Receiver<Option<String>>> {
    static LINES: OnceLock<Mutex<Receiver<Option<String>>>> = OnceLock::new();
    LINES.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || loop {
            let mut s = String::new();
            let line = match std::io::stdin().read_line(&mut s) {
                Ok(0) | Err(_) => None,
                Ok(_) => Some(s.trim().to_string()),
            };
            let end = line.is_none();
            if tx.send(line).is_err() || end {
                return;
            }
        });
        Mutex::new(rx)
    })
}

/// The next line of standard input (`None`: it ended).
pub fn read_line() -> Option<String> {
    stdin_lines().lock().unwrap_or_else(|e| e.into_inner()).recv().ok().flatten()
}

/// The process's terminal.
#[derive(Default)]
pub struct StdConsole;

impl Console for StdConsole {
    fn say(&mut self, line: &str) {
        println!("{line}");
    }
    fn ask(&mut self, prompt: &str) -> Option<String> {
        print!("{prompt}");
        let _ = std::io::stdout().flush();
        read_line()
    }
    fn poll(&mut self, wait: Duration) -> Poll {
        match stdin_lines().lock().unwrap_or_else(|e| e.into_inner()).recv_timeout(wait) {
            Ok(Some(l)) => Poll::Line(l),
            Ok(None) | Err(RecvTimeoutError::Disconnected) => Poll::Closed,
            Err(RecvTimeoutError::Timeout) => Poll::Nothing,
        }
    }
}

/// The node over JSON wRPC ([`WrpcNode`]), on the caller's runtime, with short timeouts (a person is waiting).
pub struct WrpcSetupNode<'a> {
    pub rt: &'a tokio::runtime::Runtime,
}

impl WrpcSetupNode<'_> {
    fn node(url: &str) -> WrpcNode {
        let mut c = WrpcConfig::new(url);
        c.connect_timeout = Duration::from_secs(5);
        c.request_timeout = Duration::from_secs(20);
        WrpcNode::new(c)
    }
}

impl SetupNode for WrpcSetupNode<'_> {
    fn server_info(&mut self, url: &str) -> Result<ServerInfo, String> {
        self.rt.block_on(Self::node(url).server_info()).map_err(|e| e.to_string())
    }
    fn balance(&mut self, url: &str, address: &str) -> Result<u64, String> {
        let utxos = self.rt.block_on(Self::node(url).utxos_by_addresses(&[address.to_string()])).map_err(|e| e.to_string())?;
        Ok(utxos.iter().filter(|u| u.covenant_id.is_none()).map(|u| u.amount).sum())
    }
}

/// The setup's fixed inputs.
#[derive(Debug, Clone)]
pub struct SetupOptions {
    /// Where the configuration, the key file and the data directory go.
    pub dir: PathBuf,
    /// The build's network (`None`: the reference build, which asks).
    pub build_network: Option<String>,
    /// Whether the build pins a token registry it lists without `--tokens` (`deploy-mainnet`).
    pub pinned_registry: bool,
    /// Pause between balance checks while waiting for funds.
    pub poll: Duration,
    /// The program's name in the printed command line.
    pub program: String,
}

impl SetupOptions {
    /// This build's options for `dir`.
    pub fn for_build(dir: PathBuf) -> Self {
        SetupOptions {
            dir,
            build_network: kob_protocol::artifacts::deployment_network().map(str::to_string),
            pinned_registry: kob_protocol::artifacts::deployment_registry_sha256().is_some(),
            poll: Duration::from_secs(5),
            program: program_path(),
        }
    }
}

fn ended() -> anyhow::Error {
    anyhow!("the setup's input ended before it was finished; nothing was written")
}

/// Asks with a default that Enter accepts.
fn ask_default(io: &mut dyn Console, question: &str, default: &str) -> Result<String> {
    let a = io.ask(&format!("{question} [{default}]: ")).ok_or_else(ended)?;
    Ok(if a.is_empty() { default.to_string() } else { a })
}

/// A yes / no question; Enter takes `default`.
fn ask_yes(io: &mut dyn Console, question: &str, default: bool) -> Result<bool> {
    let hint = if default { "Y/n" } else { "y/N" };
    loop {
        let a = io.ask(&format!("{question} [{hint}]: ")).ok_or_else(ended)?;
        match a.to_ascii_lowercase().as_str() {
            "" => return Ok(default),
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => io.say("Please answer y or n."),
        }
    }
}

/// One of `options` (numbered from 1); Enter takes the first.
fn ask_choice(io: &mut dyn Console, question: &str, options: &[&str]) -> Result<usize> {
    io.say(question);
    for (i, o) in options.iter().enumerate() {
        io.say(&format!("  {}) {o}", i + 1));
    }
    loop {
        let a = io.ask("Choice [1]: ").ok_or_else(ended)?;
        if a.is_empty() {
            return Ok(0);
        }
        match a.parse::<usize>() {
            Ok(n) if (1..=options.len()).contains(&n) => return Ok(n - 1),
            _ => io.say(&format!("Please answer a number from 1 to {}.", options.len())),
        }
    }
}

/// How to start a node the executor can use, for `network`.
pub fn node_hint(network: &str) -> String {
    let net = if network == "mainnet" { "" } else { "--testnet --netsuffix=10 " };
    let port = default_wrpc_url(network).unwrap_or_default();
    format!(
        "The executor needs a rusty-kaspa node (v2.1.0 or later) of {network} on this machine, with its JSON wRPC listener on \
         and a UTXO index (the operator's funds are looked up there):\n\n    kaspad {net}--utxoindex --rpclisten-json=default \
         --retention-period-days=7\n\n`--rpclisten-json=default` listens on {port} (loopback only). A new node needs some hours \
         to sync; the executor waits for it. Never expose the node's RPC to the network."
    )
}

/// The node check: what it found, and whether that blocks the role (`Err`: the operator should fix it first).
pub fn judge_node(info: &ServerInfo, network: &str, role: LaunchRole) -> Result<String, String> {
    let found = format!(
        "rusty-kaspa {} on {}, {}, UTXO index {}",
        if info.server_version.is_empty() { "?" } else { &info.server_version },
        info.network_id,
        if info.is_synced { "synced" } else { "not synced yet" },
        if info.has_utxo_index { "on" } else { "off" }
    );
    if info.network_id != network {
        return Err(format!("{found}: this is the {network} build, it cannot use a node of another network"));
    }
    if role == LaunchRole::Run && !info.has_utxo_index {
        return Err(format!("{found}: the matcher and keepers need the UTXO index (restart the node with --utxoindex)"));
    }
    Ok(found)
}

/// The interactive setup. Writes the configuration (and the key file, the token list) into `opts.dir` and returns its path
/// when the executor should start now (`None`: written, or nothing changed, and the operator starts it later).
pub fn run_setup(io: &mut dyn Console, node: &mut dyn SetupNode, opts: &SetupOptions) -> Result<Option<PathBuf>> {
    let config_path = opts.dir.join(CONFIG_FILE);
    io.say(&format!("KOB executor {} setup", kob_protocol::VERSION));
    io.say("Answer the questions below; Enter takes the default in [brackets]. Nothing is sent anywhere but to the Kaspa node you choose: no telemetry, no fee, no update check.");
    io.say("");
    let network = match &opts.build_network {
        Some(n) => {
            io.say(&format!("Network: {n} (this binary is the {n} build; another network needs the other download)."));
            n.clone()
        }
        None => loop {
            let a = io.ask("Network (this is a development build): mainnet or testnet-10: ").ok_or_else(ended)?;
            match a.as_str() {
                "mainnet" | "testnet-10" => break a,
                _ => io.say("Please type mainnet or testnet-10."),
            }
        },
    };
    if config_path.exists() {
        io.say(&format!("{} already exists.", config_path.display()));
        if !ask_yes(io, "Replace it with a new configuration?", false)? {
            io.say("Nothing changed. Start kob-executor without arguments to run the existing configuration.");
            return Ok(None);
        }
    }

    io.say("");
    let public = ask_choice(
        io,
        "Kaspa node:",
        &[
            "Public Kaspa nodes (the default, nothing to install): two independent nodes found through the Kaspa resolvers.",
            "My own node (faster; recommended for serious operators, and required for Miner Mode).",
        ],
    )? == 0;
    if public {
        io.say(PUBLIC_NODES);
    }

    io.say("");
    let role = match ask_choice(
        io,
        "What should this executor do?",
        &[
            "Matcher and keepers: match crossing orders and refund expired ones, earning the spreads and tips (needs a funded hot key).",
            "Index only: follow the chain and serve the read API (no key, no funds, no earnings).",
        ],
    )? {
        0 => LaunchRole::Run,
        _ => LaunchRole::Index,
    };

    io.say("");
    let default_url = default_wrpc_url(&network).ok_or_else(|| anyhow!("no default node endpoint for {network}"))?;
    let mut url = if public { crate::rpc::resolver::RESOLVER.to_string() } else { default_url.clone() };
    let mut node_ok = false;
    if public {
        crate::rpc::resolver::set_network(&network);
        node_ok = check_public_pair(io, node, &network, role)?;
    }
    if !public {
        loop {
            let a = io
                .ask(&format!("Your node's wRPC URL (JSON; or Borsh: a .../borsh path or port 17110 / 17210) [{url}]: "))
                .ok_or_else(ended)?;
            if a.eq_ignore_ascii_case("skip") {
                io.say("Going on without a working node: the executor will wait for one at that URL.");
                break;
            }
            if !a.is_empty() {
                url = if a.contains("://") { a } else { format!("ws://{a}") };
            }
            io.say(&format!("Checking {} ...", crate::rpc::redact_url(&url)));
            match node
                .server_info(&url)
                .map_err(|e| format!("cannot reach the node: {e}"))
                .and_then(|i| judge_node(&i, &network, role))
            {
                Ok(found) => {
                    io.say(&format!("OK: {found}."));
                    if found.contains("not synced") {
                        io.say("The executor waits until the node is synced before it does anything.");
                    }
                    node_ok = true;
                    break;
                }
                Err(why) => {
                    io.say(&format!("Problem: {why}"));
                    io.say("");
                    io.say(&node_hint(&network));
                    io.say("");
                    io.say("Press Enter to check again, type another URL, or type skip to go on anyway.");
                }
            }
        }
    }
    io.say("");
    let data_dir = ask_default(io, "Data directory (the database and the record log; a few GB)", DATA_DIR)?;

    let mut launch = LaunchConfig { role, ..LaunchConfig::default() };
    if role == LaunchRole::Run {
        io.say("");
        let key = operator_key(io, &opts.dir, &mut launch)?;
        let address = key.address(&network).to_string();
        io.say("");
        io.say(&format!("Operator address: {address}"));
        io.say("This is a HOT key: the executor signs with it unattended. Keep only working capital on it (a few days of fees and carriers), never savings.");
        io.say(&format!(
            "Back up {} somewhere only you can read: whoever has the file has the funds, and without it they are lost.",
            launch.key_file.as_deref().unwrap_or(Path::new(KEY_FILE)).display()
        ));
        io.say(&format!(
            "Send at least {MIN_FUNDS_KAS} KAS to it ({RECOMMENDED_FUNDS_KAS} KAS recommended, as a few transfers of 10 KAS or more: separate coins let several transactions go out in one tick).{}",
            if network == "mainnet" { "" } else { " Testnet KAS have no value: mine them or ask a testnet faucet." }
        ));
        if node_ok {
            wait_for_funds(io, node, &url, &address, opts.poll)?;
        }
        io.say("");
        let x402 = ask_yes(
            io,
            "Also serve the x402 facilitator (payments for other services; it needs its own configuration file, docs/ops/executor.md A.6)?",
            false,
        )?;
        if x402 {
            loop {
                let p = io.ask("Path of the x402 configuration (JSON; empty: no facilitator): ").ok_or_else(ended)?;
                if p.is_empty() {
                    break;
                }
                let full = opts.dir.join(&p);
                if full.is_file() {
                    launch.x402_config = Some(PathBuf::from(p));
                    break;
                }
                io.say(&format!("{} is not a file.", full.display()));
            }
        }
        launch.pause_file = Some(PathBuf::from(format!("{}/pause", data_dir.trim_end_matches(['/', '\\']))));
    }

    let tokens_path = if opts.pinned_registry {
        None
    } else {
        let (name, wrote) = ensure_token_list(&opts.dir, &network)?;
        let p = opts.dir.join(&name);
        if wrote {
            io.say(&format!(
                "Wrote {}: the token programs this build supports (every token of these programs is listed).",
                p.display()
            ));
        } else {
            io.say(&format!("Keeping the token list {} (delete it and run the setup again for a fresh one).", p.display()));
        }
        Some(name)
    };

    let fast_start = SetupFile::fast_start_for(launch.role);
    let lean = SetupFile::lean_for(launch.role);
    let log_file = SetupFile::log_file_for(launch.role);
    let file = SetupFile { network: network.clone(), rpc_url: url, data_dir, tokens_path, fast_start, lean, log_file, launch };
    let config_path = write_config(&opts.dir, &file)?;
    let l = launch_args(&config_path)?;
    io.say("");
    io.say(&format!("Wrote {}.", config_path.display()));
    io.say("It is the same as running (e.g. from a service):");
    io.say("");
    for line in command_line(&l, &opts.program).lines() {
        io.say(&format!("    {line}"));
    }
    io.say("");
    io.say("From now on, starting kob-executor without arguments (a double click) runs it.");
    if ask_yes(io, "Start the executor now?", true)? {
        Ok(Some(config_path))
    } else {
        Ok(None)
    }
}

/// The token list next to the configuration: kept when there, else written ([`program_registry`]). Its file name and whether
/// it was written now.
pub(crate) fn ensure_token_list(dir: &Path, network: &str) -> Result<(String, bool)> {
    let p = dir.join(TOKENS_FILE);
    if p.exists() {
        return Ok((TOKENS_FILE.to_string(), false));
    }
    std::fs::write(&p, program_registry(network)?).with_context(|| p.display().to_string())?;
    Ok((TOKENS_FILE.to_string(), true))
}

/// Writes `file` as `dir`/[`CONFIG_FILE`], after checking that it reads back as a launch configuration; returns its path.
pub(crate) fn write_config(dir: &Path, file: &SetupFile) -> Result<PathBuf> {
    let config_path = dir.join(CONFIG_FILE);
    let text = file.text()?;
    // the file must be what a start without arguments reads back
    let tmp = dir.join(format!("{CONFIG_FILE}.tmp"));
    std::fs::write(&tmp, &text).with_context(|| tmp.display().to_string())?;
    let check = launch_args(&tmp);
    if let Err(e) = check {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.context("the configuration written does not read back"));
    }
    std::fs::rename(&tmp, &config_path).with_context(|| config_path.display().to_string())?;
    Ok(config_path)
}

/// What public nodes mean for an operator (the setup's note under the choice).
pub const PUBLIC_NODES: &str = "Public nodes are shared and slower than your own. A bad node cannot steal funds: the executor signs only its own inputs and the covenants enforce every order. It could waste fees or hide orders, so the executor always uses two public nodes on different hosts, indexes only what both report, and sends every transaction to both; if one fails, the other carries on and a replacement is found.";

/// The check of the public pair (`resolver` and `resolver:2`, `rpc::resolver`): both resolved on different hosts, each on the
/// build's network, synced or syncing, with the UTXO index the role needs. `Ok(true)`: both fine; `Ok(false)`: the operator
/// goes on anyway (the executor keeps resolving).
fn check_public_pair(io: &mut dyn Console, node: &mut dyn SetupNode, network: &str, role: LaunchRole) -> Result<bool> {
    loop {
        io.say("Finding two public nodes through the Kaspa resolvers ...");
        let mut problems = vec![];
        for (entry, what) in [(crate::rpc::resolver::pseudo(1), "chain authority"), (crate::rpc::resolver::pseudo(2), "cross-check")] {
            let res = node
                .server_info(&entry)
                .map_err(|e| format!("cannot reach a public node: {e}"))
                .and_then(|i| judge_node(&i, network, role));
            let name = node.node_name(&entry).unwrap_or_else(|| entry.clone());
            match res {
                Ok(found) => io.say(&format!("OK: {name} ({what}): {found}.")),
                Err(why) => problems.push(format!("{name} ({what}): {why}")),
            }
        }
        if problems.is_empty() {
            return Ok(true);
        }
        for p in &problems {
            io.say(&format!("Problem: {p}"));
        }
        io.say("Press Enter to try again (other nodes are tried), or type skip to go on anyway.");
        let a = io.ask("> ").ok_or_else(ended)?;
        if a.eq_ignore_ascii_case("skip") {
            io.say("Going on: the executor keeps looking for public nodes when it starts.");
            return Ok(false);
        }
    }
}

/// The operator's key: the existing key file next to the configuration, a new one, or a file the operator names. Sets
/// `launch.key_file`. Never prints the secret.
fn operator_key(io: &mut dyn Console, dir: &Path, launch: &mut LaunchConfig) -> Result<HotKey> {
    let own = dir.join(KEY_FILE);
    if own.exists() {
        io.say(&format!("Found the key file {}.", own.display()));
        if ask_yes(io, "Use it (the funds on it stay usable)?", true)? {
            let k = HotKey::from_file(&own)?;
            launch.key_file = Some(PathBuf::from(KEY_FILE));
            return Ok(k);
        }
        return existing_key(io, dir, launch);
    }
    match ask_choice(
        io,
        "The operator's hot key:",
        &[
            &format!("Generate a new key into {KEY_FILE} (readable by this account only)"),
            "Use an existing key file (64 hex characters)",
        ],
    )? {
        0 => {
            let k = HotKey::create_file(&own)?;
            io.say(&format!("Generated a new key in {} (only this account can read it).", own.display()));
            launch.key_file = Some(PathBuf::from(KEY_FILE));
            Ok(k)
        }
        _ => existing_key(io, dir, launch),
    }
}

fn existing_key(io: &mut dyn Console, dir: &Path, launch: &mut LaunchConfig) -> Result<HotKey> {
    loop {
        let p = io.ask("Path of the key file: ").ok_or_else(ended)?;
        if p.is_empty() {
            continue;
        }
        match HotKey::from_file(&dir.join(&p)) {
            Ok(k) => {
                launch.key_file = Some(PathBuf::from(p));
                return Ok(k);
            }
            Err(e) => io.say(&format!("Cannot use it: {e}")),
        }
    }
}

/// Shows the address's balance until it reaches [`MIN_FUNDS_KAS`] or the operator presses Enter.
fn wait_for_funds(io: &mut dyn Console, node: &mut dyn SetupNode, url: &str, address: &str, every: Duration) -> Result<()> {
    io.say(&format!("Waiting for the funds (checked every {} s). Press Enter to go on without waiting.", every.as_secs().max(1)));
    let mut last = None;
    loop {
        match node.balance(url, address) {
            Ok(b) => {
                if last != Some(b) {
                    io.say(&format!("Balance: {} KAS", kas_text(b as i64)));
                    last = Some(b);
                }
                if b >= MIN_FUNDS_KAS * 100_000_000 {
                    io.say("Funded.");
                    return Ok(());
                }
            }
            Err(e) => {
                io.say(&format!("Cannot read the balance ({e}); not waiting. The executor reports its funds once it runs."));
                return Ok(());
            }
        }
        match io.poll(every) {
            Poll::Nothing => {}
            Poll::Line(_) => {
                match last {
                    // funds are there, only fewer than the recommended minimum: say so instead of "not waiting"
                    Some(b) if b > 0 => io.say(&format!(
                        "Going on with the {} KAS there; the executor reports its funds and warns below {MIN_FUNDS_KAS} KAS.",
                        kas_text(b as i64)
                    )),
                    _ => io.say("Not waiting for the funds: the executor starts with what is there and reports its funds."),
                }
                return Ok(());
            }
            Poll::Closed => return Err(ended()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Scripted answers; `"<wait>"` is a poll that times out.
    struct Script {
        answers: VecDeque<String>,
        out: Vec<String>,
    }

    impl Script {
        fn new(answers: &[&str]) -> Self {
            Script { answers: answers.iter().map(|s| s.to_string()).collect(), out: vec![] }
        }
        fn text(&self) -> String {
            self.out.join("\n")
        }
    }

    impl Console for Script {
        fn say(&mut self, line: &str) {
            self.out.push(line.to_string());
        }
        fn ask(&mut self, prompt: &str) -> Option<String> {
            self.out.push(prompt.to_string());
            self.answers.pop_front()
        }
        fn poll(&mut self, _wait: Duration) -> Poll {
            match self.answers.pop_front() {
                Some(a) if a == "<wait>" => Poll::Nothing,
                Some(a) => Poll::Line(a),
                None => Poll::Closed,
            }
        }
    }

    /// A node that answers `info` (or refuses until `refuse` calls have been made) and reports the balances in turn.
    struct FakeNode {
        info: ServerInfo,
        refuse: usize,
        balances: VecDeque<u64>,
        urls: Vec<String>,
    }

    impl FakeNode {
        fn new(network: &str) -> Self {
            FakeNode {
                info: ServerInfo {
                    server_version: "2.1.0".into(),
                    network_id: network.into(),
                    is_synced: true,
                    has_utxo_index: true,
                    virtual_daa_score: 1,
                },
                refuse: 0,
                balances: VecDeque::new(),
                urls: vec![],
            }
        }
    }

    impl SetupNode for FakeNode {
        fn server_info(&mut self, url: &str) -> Result<ServerInfo, String> {
            self.urls.push(url.to_string());
            if self.refuse > 0 {
                self.refuse -= 1;
                return Err("transport: connection refused".into());
            }
            Ok(self.info.clone())
        }
        fn balance(&mut self, _url: &str, _address: &str) -> Result<u64, String> {
            Ok(self.balances.pop_front().unwrap_or(0))
        }
        fn node_name(&mut self, url: &str) -> Option<String> {
            crate::rpc::resolver::is_resolver(url).then(|| format!("{}.public.example", url.replace(':', "-")))
        }
    }

    fn opts(dir: &Path, network: &str, pinned: bool) -> SetupOptions {
        SetupOptions {
            dir: dir.to_path_buf(),
            build_network: Some(network.into()),
            pinned_registry: pinned,
            poll: Duration::ZERO,
            program: "kob-executor".into(),
        }
    }

    #[test]
    fn no_arguments_run_the_configuration_or_the_setup() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(no_args_start(d.path(), true), NoArgs::Setup);
        assert!(matches!(no_args_start(d.path(), false), NoArgs::Refuse(m) if m.contains("kob-executor setup")));
        std::fs::write(d.path().join(CONFIG_FILE), "").unwrap();
        assert_eq!(no_args_start(d.path(), true), NoArgs::Launch(d.path().join(CONFIG_FILE)));
        assert_eq!(no_args_start(d.path(), false), NoArgs::Launch(d.path().join(CONFIG_FILE)));
    }

    #[test]
    fn the_directory_is_the_environments_or_the_executables() {
        assert_eq!(home_dir_from(Some("/data".into())).unwrap(), PathBuf::from("/data"));
        assert_eq!(home_dir_from(Some("".into())).unwrap(), exe_dir().unwrap());
        assert_eq!(home_dir_from(None).unwrap(), exe_dir().unwrap());
    }

    #[test]
    fn the_default_node_is_the_local_node_of_the_build() {
        // rusty-kaspa v2.1.0 `--rpclisten-json=default`: 127.0.0.1 and the network's JSON wRPC port
        assert_eq!(default_wrpc_url("testnet-10").as_deref(), Some("ws://127.0.0.1:18210"));
        assert_eq!(default_wrpc_url("mainnet").as_deref(), Some("ws://127.0.0.1:18110"));
        assert!(node_hint("testnet-10").contains("kaspad --testnet --netsuffix=10 --utxoindex --rpclisten-json=default"));
        assert!(node_hint("mainnet").contains("kaspad --utxoindex --rpclisten-json=default"));
        assert!(node_hint("mainnet").contains("ws://127.0.0.1:18110"));
    }

    #[test]
    fn the_configuration_round_trips_into_the_command_line() {
        let d = tempfile::tempdir().unwrap();
        let f = SetupFile {
            network: "testnet-10".into(),
            rpc_url: "ws://127.0.0.1:18210".into(),
            data_dir: "kob-data".into(),
            tokens_path: Some(TOKENS_FILE.into()),
            fast_start: SetupFile::fast_start_for(LaunchRole::Run),
            lean: SetupFile::lean_for(LaunchRole::Run),
            log_file: SetupFile::log_file_for(LaunchRole::Run),
            launch: LaunchConfig {
                role: LaunchRole::Run,
                key_file: Some(KEY_FILE.into()),
                pause_file: Some("kob-data/pause".into()),
                x402_config: None,
                args: vec!["--sweep-own-strays".into()],
            },
        };
        let p = d.path().join(CONFIG_FILE);
        std::fs::write(&p, f.text().unwrap()).unwrap();
        let c = IndexerConfig::load(&p).expect("a valid --config file");
        assert_eq!((c.network.as_str(), c.rpc_url.as_str()), ("testnet-10", "ws://127.0.0.1:18210"));
        assert_eq!(c.data_dir, PathBuf::from("kob-data"));
        assert_eq!(c.tokens_path, Some(PathBuf::from(TOKENS_FILE)));
        // a matcher's configuration trades within seconds of the first start; an index-only one has no use for it
        assert!(c.fast_start && SetupFile::fast_start_for(LaunchRole::Index).is_none());
        assert!(c.lean && SetupFile::lean_for(LaunchRole::Index).is_none());
        // a matcher keeps a rotated log file next to the configuration (a scheduled task has no console); index only does not
        assert_eq!(c.log_file, Some(PathBuf::from(LOG_FILE)));
        assert!((c.log_max_mb, c.log_files) == (10, 5) && SetupFile::log_file_for(LaunchRole::Index).is_none());
        assert_eq!(c.launch.as_ref(), Some(&f.launch));
        let l = launch_args(&p).unwrap();
        assert_eq!(l.dir, d.path());
        assert_eq!(
            l.args,
            ["run", "--config", CONFIG_FILE, "--key-file", KEY_FILE, "--pause-file", "kob-data/pause", "--sweep-own-strays"]
        );
        assert_eq!(l.api, Some("127.0.0.1:8090".parse().unwrap()));
        let cl = command_line(&l, "kob-executor.exe");
        assert!(
            cl.ends_with(
                "\nkob-executor.exe run --config kob-executor.toml --key-file kob-operator.key --pause-file kob-data/pause \
                 --sweep-own-strays"
            ),
            "{cl}"
        );
        // index only: no key options; a key option there is refused, not dropped
        let idx = SetupFile { launch: LaunchConfig { role: LaunchRole::Index, ..LaunchConfig::default() }, ..f.clone() };
        std::fs::write(&p, idx.text().unwrap()).unwrap();
        assert_eq!(launch_args(&p).unwrap().args, ["index", "--config", CONFIG_FILE]);
        std::fs::write(&p, idx.text().unwrap().replace("role = \"index\"", "role = \"index\"\nkey_file = \"k\"")).unwrap();
        assert!(launch_args(&p).unwrap_err().to_string().contains("key_file"));
        // a misspelt launch key is refused like any other
        std::fs::write(&p, f.text().unwrap().replace("pause_file", "pausefile")).unwrap();
        assert!(launch_args(&p).is_err());
        // a plain --config file is not a launch configuration
        std::fs::write(&p, "network = \"testnet-10\"\n").unwrap();
        assert!(launch_args(&p).unwrap_err().to_string().contains("[launch]"));
        assert_eq!(shell_word("C:\\Program Files\\kob"), "\"C:\\Program Files\\kob\"");
    }

    #[test]
    fn the_testnet_program_list_is_open_and_names_no_mainnet_token() {
        let text = program_registry("testnet-10").unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["network"], "testnet-10");
        assert_eq!(v["tokens"], serde_json::json!([]));
        assert!(!v["templates"].as_array().unwrap().is_empty());
        assert!(crate::tokens::TokenAllowlist::parse(&text).unwrap().is_open());
    }

    /// The whole dialogue of a testnet operator taking every default: a refused node first, then the node, a new key, funds
    /// arriving, no x402, start.
    #[test]
    fn a_first_setup_with_the_defaults() {
        let d = tempfile::tempdir().unwrap();
        let mut node = FakeNode::new("testnet-10");
        node.refuse = 1;
        node.balances = [0, 0, 150 * 100_000_000].into();
        // node (public), role, Enter after the refused check, data dir, key choice, two polls while waiting, x402, start
        let mut io = Script::new(&["", "", "", "", "", "<wait>", "<wait>", "", ""]);
        let got = run_setup(&mut io, &mut node, &opts(d.path(), "testnet-10", false)).unwrap();
        let out = io.text();
        assert_eq!(got, Some(d.path().join(CONFIG_FILE)), "{out}");
        assert_eq!(node.urls, ["resolver", "resolver:2", "resolver", "resolver:2"], "both public nodes, checked again");
        assert!(out.contains("Network: testnet-10"), "{out}");
        assert!(out.contains("A bad node cannot steal funds") && out.contains("two public nodes on different hosts"), "{out}");
        assert!(out.contains("Problem: resolver.public.example (chain authority): cannot reach a public node"), "{out}");
        assert!(
            out.contains("OK: resolver.public.example (chain authority): rusty-kaspa 2.1.0 on testnet-10, synced, UTXO index on"),
            "{out}"
        );
        assert!(out.contains("OK: resolver-2.public.example (cross-check)"), "{out}");
        assert!(out.contains("Operator address: kaspatest:") && out.contains("HOT key"), "{out}");
        assert!(out.contains("Balance: 0 KAS") && out.contains("Balance: 150 KAS") && out.contains("Funded."), "{out}");
        // the secret is never shown
        let secret = std::fs::read_to_string(d.path().join(KEY_FILE)).unwrap();
        assert!(!out.contains(secret.trim()), "the setup printed the secret");
        let k = HotKey::from_file(&d.path().join(KEY_FILE)).unwrap();
        assert!(out.contains(&k.address("testnet-10").to_string()));
        // the configuration, the token list, and the command line it stands for
        let l = launch_args(&d.path().join(CONFIG_FILE)).unwrap();
        assert_eq!(l.args, ["run", "--config", CONFIG_FILE, "--key-file", KEY_FILE, "--pause-file", "kob-data/pause"]);
        let c = IndexerConfig::load(&d.path().join(CONFIG_FILE)).unwrap();
        assert_eq!(c.rpc_url, "resolver", "public nodes (index_cli adds the second, resolver:2)");
        assert_eq!(c.tokens_path, Some(PathBuf::from(TOKENS_FILE)));
        assert!(d.path().join(TOKENS_FILE).is_file());
        assert!(out.contains("kob-executor run --config kob-executor.toml --key-file kob-operator.key"), "{out}");
        assert!(!d.path().join(format!("{CONFIG_FILE}.tmp")).exists());

        // again: the configuration is kept unless replaced
        let mut io = Script::new(&[""]);
        assert_eq!(run_setup(&mut io, &mut node, &opts(d.path(), "testnet-10", false)).unwrap(), None);
        assert!(io.text().contains("Nothing changed"));
        // replaced: the existing key is offered (never a new one over it), index only this time
        let mut io = Script::new(&["y", "", "2", "", "n"]);
        assert_eq!(run_setup(&mut io, &mut node, &opts(d.path(), "testnet-10", false)).unwrap(), None);
        assert_eq!(launch_args(&d.path().join(CONFIG_FILE)).unwrap().args, ["index", "--config", CONFIG_FILE]);
        assert_eq!(std::fs::read_to_string(d.path().join(KEY_FILE)).unwrap(), secret, "the key file is untouched");
        let mut io = Script::new(&["y", "", "", "", "", "<enter>", "", "n"]);
        // (replace, public nodes, role, data dir, use the key, Enter while waiting, x402, start)
        node.balances = [30 * 100_000_000].into();
        assert_eq!(run_setup(&mut io, &mut node, &opts(d.path(), "testnet-10", false)).unwrap(), None);
        let out = io.text();
        assert!(out.contains("Found the key file") && out.contains("Going on with the 30 KAS there"), "{out}");
        assert!(!out.contains("Not waiting for the funds"), "funds had arrived: {out}");
        assert!(out.contains(&k.address("testnet-10").to_string()), "the same key: {out}");
    }

    /// Enter while waiting: with nothing there the setup says it is not waiting, with some funds it says it goes on with them.
    #[test]
    fn going_on_without_waiting_tells_whether_funds_arrived() {
        let mut node = FakeNode::new("testnet-10");
        node.balances = [0].into();
        let mut io = Script::new(&["<enter>"]);
        wait_for_funds(&mut io, &mut node, "resolver", "kaspatest:x", Duration::from_secs(1)).unwrap();
        assert!(io.text().contains("Not waiting for the funds"), "{}", io.text());
        let mut node = FakeNode::new("testnet-10");
        node.balances = [30 * 100_000_000].into();
        let mut io = Script::new(&["<enter>"]);
        wait_for_funds(&mut io, &mut node, "resolver", "kaspatest:x", Duration::from_secs(1)).unwrap();
        assert!(io.text().contains("Going on with the 30 KAS there") && !io.text().contains("Not waiting"), "{}", io.text());
    }

    #[test]
    fn a_node_of_the_wrong_network_or_without_utxo_index_is_explained() {
        let mut info = FakeNode::new("mainnet").info;
        assert!(judge_node(&info, "testnet-10", LaunchRole::Run).unwrap_err().contains("another network"));
        info.network_id = "testnet-10".into();
        info.has_utxo_index = false;
        assert!(judge_node(&info, "testnet-10", LaunchRole::Run).unwrap_err().contains("--utxoindex"));
        assert!(judge_node(&info, "testnet-10", LaunchRole::Index).is_ok(), "the index role needs no UTXO index");
        info.is_synced = false;
        assert!(judge_node(&info, "testnet-10", LaunchRole::Index).unwrap().contains("not synced yet"));

        // mainnet build: another URL (scheme added), skip the balance wait, an existing key file of the operator's, x402 with
        // a file, no token list (the build pins its registry)
        let d = tempfile::tempdir().unwrap();
        let mut node = FakeNode::new("mainnet");
        node.info.has_utxo_index = false;
        let key = tempfile::tempdir().unwrap();
        let kp = key.path().join("my.key");
        let made = HotKey::create_file(&kp).unwrap();
        std::fs::write(d.path().join("x402.json"), "{}").unwrap();
        let kps = kp.to_string_lossy().into_owned();
        let mut io =
            Script::new(&["2", "", "10.0.0.5:18110", "skip", "data", "2", "missing.key", &kps, "y", "nope.json", "x402.json", ""]);
        let got = run_setup(&mut io, &mut node, &opts(d.path(), "mainnet", true)).unwrap();
        let out = io.text();
        assert_eq!(got, Some(d.path().join(CONFIG_FILE)), "{out}");
        assert_eq!(node.urls, ["ws://10.0.0.5:18110"]);
        assert!(out.contains("Going on without a working node") && !out.contains("Waiting for the funds"), "{out}");
        assert!(out.contains("Cannot use it") && out.contains("is not a file"), "{out}");
        assert!(out.contains(&made.address("mainnet").to_string()), "{out}");
        assert!(!d.path().join(TOKENS_FILE).exists());
        let c = IndexerConfig::load(&d.path().join(CONFIG_FILE)).unwrap();
        assert_eq!((c.network.as_str(), c.rpc_url.as_str(), c.tokens_path.clone()), ("mainnet", "ws://10.0.0.5:18110", None));
        let l = launch_args(&d.path().join(CONFIG_FILE)).unwrap();
        assert_eq!(
            l.args,
            ["run", "--config", CONFIG_FILE, "--key-file", kps.as_str(), "--pause-file", "data/pause", "--x402-config", "x402.json"]
        );
    }

    #[test]
    fn input_that_ends_early_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let mut node = FakeNode::new("testnet-10");
        let mut io = Script::new(&["", ""]);
        let e = run_setup(&mut io, &mut node, &opts(d.path(), "testnet-10", false)).unwrap_err();
        assert!(e.to_string().contains("input ended"), "{e}");
        assert!(!d.path().join(CONFIG_FILE).exists());
        // the questions repeat on a wrong answer
        let mut io = Script::new(&["7", "x"]);
        assert!(run_setup(&mut io, &mut node, &opts(d.path(), "testnet-10", false)).is_err());
        assert!(io.text().contains("Please answer a number from 1 to 2."));
    }
}
