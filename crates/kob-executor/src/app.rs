//! The non-interactive path for a wrapper that cannot answer the setup's questions (the Android app):
//! `kobd setup --non-interactive ...` writes the same files as the interactive setup and prints one JSON object,
//! and `kobd address-balance` prints the operator's address and its plain balance as one JSON object.
//!
//! Neither prints the secret key. Neither talks to anything but the node the configuration names (no telemetry, no fee, no
//! project server).

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::config::{default_wrpc_url, IndexerConfig, LaunchConfig, LaunchRole};
use crate::matcher::run::kas_text;
use crate::matcher::wallet::HotKey;
use crate::setup::{
    ensure_token_list, write_config, SetupFile, SetupNode, SetupOptions, WrpcSetupNode, CONFIG_FILE, DATA_DIR, KEY_FILE,
};

/// The roles of a `kobd` process, as the setup and the role flags name them.
///
/// * **Executor**: matches crossing orders and refunds expired ones with a funded hot key (`run`); lean storage and fast start.
/// * **Indexer**: follows the chain, keeps the full history and the record log and serves the read API (`index`; no key).
/// * **Executor and Indexer**: both in one process (`run` over an archive store: `lean = false`). The public API and the
///   signing key then live on the same machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppRole {
    Executor,
    Indexer,
    ExecutorIndexer,
}

/// The warning shown wherever the combined role is offered.
pub const COMBINED_WARNING: &str =
    "the public API and the signing key then live on the same machine; for a public server prefer separate machines";

/// `--executor` and `--indexer`: kaspad-style role flags of the setup and of a start (`kobd --executor --indexer`).
#[derive(clap::Args, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoleFlags {
    /// The Executor role: match crossing orders and refund expired ones (needs a funded hot key). Lean storage and fast start;
    /// a full store with --indexer.
    #[arg(long)]
    pub executor: bool,
    /// The Indexer role: follow the chain, keep the full history and the record log and serve the read API (no key). With
    /// --executor, one process does both (the public API and the signing key then live on the same machine).
    #[arg(long)]
    pub indexer: bool,
}

impl RoleFlags {
    /// The roles the flags name (`None`: neither flag).
    pub fn role(self) -> Option<AppRole> {
        match (self.executor, self.indexer) {
            (true, true) => Some(AppRole::ExecutorIndexer),
            (true, false) => Some(AppRole::Executor),
            (false, true) => Some(AppRole::Indexer),
            (false, false) => None,
        }
    }
}

impl AppRole {
    /// The roles as `roles` of the setup's output and `/v1/health` name them: `executor` first.
    pub fn names(self) -> &'static [&'static str] {
        match self {
            AppRole::Executor => &["executor"],
            AppRole::Indexer => &["indexer"],
            AppRole::ExecutorIndexer => &["executor", "indexer"],
        }
    }

    /// The roles in words, for the banners and the configuration's header.
    pub fn display(self) -> &'static str {
        match self {
            AppRole::Executor => "Executor",
            AppRole::Indexer => "Indexer",
            AppRole::ExecutorIndexer => "Executor and Indexer",
        }
    }

    /// The subcommand the role runs: `run` for every role with a key.
    pub fn launch_role(self) -> LaunchRole {
        match self {
            AppRole::Executor | AppRole::ExecutorIndexer => LaunchRole::Run,
            AppRole::Indexer => LaunchRole::Index,
        }
    }

    /// `true` for the roles that sign with the operator's key.
    pub fn has_key(self) -> bool {
        self.launch_role() == LaunchRole::Run
    }

    /// `true` for the roles that keep the whole history and the record log (an archive).
    pub fn is_archive(self) -> bool {
        matches!(self, AppRole::Indexer | AppRole::ExecutorIndexer)
    }

    /// The roles a launch configuration stands for: `index` is the Indexer; `run` with lean storage is the Executor and `run`
    /// over a full store (`lean = false`) is both.
    pub fn from_parts(role: LaunchRole, lean: bool) -> AppRole {
        match (role, lean) {
            (LaunchRole::Index, _) => AppRole::Indexer,
            (LaunchRole::Run, true) => AppRole::Executor,
            (LaunchRole::Run, false) => AppRole::ExecutorIndexer,
        }
    }

    /// The roles of the configuration `cfg` (`None`: a plain `--config` file with no `[launch]` table).
    pub fn of_config(cfg: &IndexerConfig) -> Option<AppRole> {
        cfg.launch.as_ref().map(|l| AppRole::from_parts(l.role, cfg.lean))
    }

    /// The role flags of the roles, as a start names them: `--executor`, `--indexer` or `--executor --indexer`.
    pub fn flags(self) -> &'static [&'static str] {
        match self {
            AppRole::Executor => &["--executor"],
            AppRole::Indexer => &["--indexer"],
            AppRole::ExecutorIndexer => &["--executor", "--indexer"],
        }
    }
}

/// What `setup --non-interactive` was asked for.
#[derive(Debug, Clone)]
pub struct AppSetup {
    /// `resolver` (two public nodes found through the Kaspa resolvers) or a node's wRPC URL.
    pub node: String,
    pub role: AppRole,
    /// Make the operator key when the directory has none (an existing key file is always kept, never replaced).
    pub new_key: bool,
    /// Needed by a build with no deployment network; a deployment build accepts only its own.
    pub network: Option<String>,
}

/// The node entry of the configuration for `--node`: `resolver` (any case) is the public pair, a URL is used as given (a
/// bare `host:port` gets `ws://`).
pub fn node_entry(node: &str) -> Result<String> {
    let n = node.trim();
    if n.is_empty() {
        bail!("--node is empty: give `resolver` or the URL of your node");
    }
    if n.eq_ignore_ascii_case(crate::rpc::resolver::RESOLVER) {
        return Ok(crate::rpc::resolver::RESOLVER.to_string());
    }
    Ok(if n.contains("://") { n.to_string() } else { format!("ws://{n}") })
}

/// The non-interactive setup into `opts.dir`: writes the configuration (replacing one), the token list when the build pins no
/// registry (kept when there) and, for the roles with a key, the key file when there is none and `new_key` says to make one.
/// Returns what the wrapper needs as a JSON object: `address` (null for the Indexer alone), `config`, `key_file`, `network`,
/// `role` (the subcommand), `roles` (`["executor"]`, `["indexer"]` or `["executor", "indexer"]`), `node`, `data_dir`. The secret is never
/// part of it.
pub fn setup_non_interactive(opts: &SetupOptions, a: &AppSetup) -> Result<Value> {
    let network = match (&opts.build_network, a.network.as_deref()) {
        (Some(b), Some(g)) if b != g => bail!("this binary is the {b} build, it cannot set up {g}"),
        (Some(b), _) => b.clone(),
        (None, Some(g)) if matches!(g, "mainnet" | "testnet-10") => g.to_string(),
        (None, Some(g)) => bail!("--network must be mainnet or testnet-10, not {g}"),
        (None, None) => bail!("this is a development build with no deployment network: pass --network mainnet|testnet-10"),
    };
    let url = node_entry(&a.node)?;
    default_wrpc_url(&network).ok_or_else(|| anyhow!("no default node endpoint for {network}"))?;
    let role = a.role.launch_role();
    let key_path = opts.dir.join(KEY_FILE);
    if a.role.has_key() && !key_path.exists() && !a.new_key {
        bail!("the Executor role needs a key and {} has none: pass --new-key", key_path.display());
    }
    std::fs::create_dir_all(&opts.dir).with_context(|| opts.dir.display().to_string())?;

    let mut launch = LaunchConfig { role, ..LaunchConfig::default() };
    let data_dir = DATA_DIR.to_string();
    let mut address = Value::Null;
    if a.role.has_key() {
        // an existing key file is used, never replaced
        let key = if key_path.exists() { HotKey::from_file(&key_path)? } else { HotKey::create_file(&key_path)? };
        address = json!(key.address(&network).to_string());
        launch.key_file = Some(PathBuf::from(KEY_FILE));
        launch.pause_file = Some(PathBuf::from(format!("{data_dir}/pause")));
    }
    let tokens_path = if opts.pinned_registry { None } else { Some(ensure_token_list(&opts.dir, &network)?.0) };
    let fast_start = SetupFile::fast_start_for(a.role);
    let lean = SetupFile::lean_for(a.role);
    let file = SetupFile {
        network: network.clone(),
        rpc_url: url.clone(),
        data_dir: data_dir.clone(),
        tokens_path,
        fast_start,
        lean,
        log_file: None,
        launch,
    };
    let config = write_config(&opts.dir, &file)?;
    Ok(json!({
        "address": address,
        "config": config.to_string_lossy(),
        "key_file": file.launch.key_file.as_ref().map(|k| opts.dir.join(k).to_string_lossy().into_owned()),
        "network": network,
        "role": role.subcommand(),
        "roles": a.role.names(),
        "node": crate::rpc::redact_url(&url),
        "data_dir": opts.dir.join(&data_dir).to_string_lossy(),
    }))
}

/// The operator's address of the configuration at `cfg_path` (the key file is relative to the configuration's directory): what the
/// start banner shows, so the funding address is never lost after the setup. `None`: an Indexer-only configuration, or a key
/// that cannot be read (the run itself reports that). Never the secret.
pub fn operator_address(cfg_path: &Path) -> Option<String> {
    let cfg = IndexerConfig::load(cfg_path).ok()?;
    let key_file = cfg.launch.as_ref()?.key_file.clone()?;
    let network = crate::config::resolve_network(Some(&cfg.network)).ok()?;
    let dir = cfg_path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let path = if key_file.is_absolute() { key_file } else { dir.join(key_file) };
    Some(HotKey::from_file(&path).ok()?.address(&network).to_string())
}

/// `kobd address-balance`.
#[derive(clap::Args, Debug, Clone)]
pub struct AddressBalanceArgs {
    /// Directory of `kob-executor.toml` and the key file. Default: `$KOB_EXECUTOR_DIR` or the executable's own directory.
    #[arg(long)]
    pub dir: Option<PathBuf>,
}

/// The operator's address of the configuration in `dir` and its plain balance (covenant-free UTXOs) from the configured node:
/// `{address, network, balance_sompi, balance_kas}`, or, when the node cannot be read, `balance_sompi: null` and
/// `error`. An Indexer-only configuration has no key: an error. Never prints the secret.
pub fn address_balance(dir: &Path, node: &mut dyn SetupNode) -> Result<Value> {
    let cfg_path = dir.join(CONFIG_FILE);
    let cfg = IndexerConfig::load(&cfg_path)?;
    let key_file = cfg
        .launch
        .as_ref()
        .and_then(|l| l.key_file.clone())
        .ok_or_else(|| anyhow!("{} has no operator key (Indexer-only configuration): there is no address", cfg_path.display()))?;
    let network = crate::config::resolve_network(Some(&cfg.network)).map_err(|e| anyhow!(e))?;
    let address = HotKey::from_file(&dir.join(key_file))?.address(&network).to_string();
    let url = if cfg.rpc_url.is_empty() {
        default_wrpc_url(&network).ok_or_else(|| anyhow!("no default node endpoint for {network}"))?
    } else {
        cfg.rpc_url.clone()
    };
    if crate::rpc::resolver::is_resolver(&url) {
        crate::rpc::resolver::set_network(&network);
    }
    Ok(match node.balance(&url, &address) {
        Ok(b) => json!({"address": address, "network": network, "balance_sompi": b, "balance_kas": kas_text(b as i64)}),
        Err(e) => json!({"address": address, "network": network, "balance_sompi": null, "balance_kas": null, "error": e}),
    })
}

/// [`address_balance`] on the real node, on the caller's runtime.
pub fn address_balance_live(dir: &Path, rt: &tokio::runtime::Runtime) -> Result<Value> {
    address_balance(dir, &mut WrpcSetupNode { rt })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matcher::node::ServerInfo;
    use std::time::Duration;

    struct Fake(Result<u64, String>);

    impl SetupNode for Fake {
        fn server_info(&mut self, _url: &str) -> Result<ServerInfo, String> {
            Err("not used".into())
        }
        fn balance(&mut self, _url: &str, _address: &str) -> Result<u64, String> {
            self.0.clone()
        }
    }

    fn opts(dir: &Path, network: Option<&str>, pinned: bool) -> SetupOptions {
        SetupOptions {
            dir: dir.to_path_buf(),
            build_network: network.map(str::to_string),
            pinned_registry: pinned,
            poll: Duration::ZERO,
            program: "kobd".into(),
        }
    }

    fn ask(node: &str, role: AppRole, new_key: bool) -> AppSetup {
        AppSetup { node: node.into(), role, new_key, network: None }
    }

    #[test]
    fn the_node_entry_is_the_resolver_or_a_url() {
        assert_eq!(node_entry("resolver").unwrap(), "resolver");
        assert_eq!(node_entry(" Resolver ").unwrap(), "resolver");
        assert_eq!(node_entry("10.0.0.5:18210").unwrap(), "ws://10.0.0.5:18210");
        assert_eq!(node_entry("wss://node.example/x").unwrap(), "wss://node.example/x");
        assert!(node_entry("  ").is_err());
    }

    #[test]
    fn an_executor_setup_writes_the_files_and_reports_the_address_but_not_the_secret() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("files").join("kob");
        let tn = Some("testnet-10");
        let out = setup_non_interactive(&opts(&dir, tn, false), &ask("resolver", AppRole::Executor, true)).unwrap();
        let key_path = dir.join(KEY_FILE);
        let key = HotKey::from_file(&key_path).unwrap();
        assert_eq!(out["address"], key.address("testnet-10").to_string());
        assert!(out["address"].as_str().unwrap().starts_with("kaspatest:"));
        assert_eq!(
            (out["role"].as_str(), out["network"].as_str(), out["node"].as_str()),
            (Some("run"), Some("testnet-10"), Some("resolver"))
        );
        assert_eq!(out["config"], dir.join(CONFIG_FILE).to_string_lossy().as_ref());
        let secret = std::fs::read_to_string(&key_path).unwrap();
        assert!(!out.to_string().contains(secret.trim()), "the secret is never printed");
        // the configuration is what a start without arguments reads
        let l = crate::setup::launch_args(&dir.join(CONFIG_FILE)).unwrap();
        assert_eq!(l.args, ["--config", CONFIG_FILE, "--key-file", KEY_FILE, "--pause-file", "kob-data/pause"]);
        let c = IndexerConfig::load(&dir.join(CONFIG_FILE)).unwrap();
        assert_eq!((c.network.as_str(), c.rpc_url.as_str()), ("testnet-10", "resolver"));
        assert!(c.fast_start, "the app's first start trades within seconds (docs/ops/executor.md, Fast start)");
        assert!(c.lean, "the app keeps the live book, not the history (docs/ops/executor.md, Lean storage)");
        assert!(dir.join(crate::setup::TOKENS_FILE).is_file());
        assert!(!dir.join(format!("{CONFIG_FILE}.tmp")).exists());

        // again with another node: the configuration is replaced, the key is the same file, untouched
        let out2 = setup_non_interactive(&opts(&dir, tn, false), &ask("10.0.0.5:18210", AppRole::Executor, true)).unwrap();
        assert_eq!(out2["address"], out["address"]);
        assert_eq!(std::fs::read_to_string(&key_path).unwrap(), secret);
        assert_eq!(IndexerConfig::load(&dir.join(CONFIG_FILE)).unwrap().rpc_url, "ws://10.0.0.5:18210");
        // without --new-key an existing key is used too
        let out3 = setup_non_interactive(&opts(&dir, tn, false), &ask("resolver", AppRole::Executor, false)).unwrap();
        assert_eq!(out3["address"], out["address"]);
        // index only: no key options, no address
        let out4 = setup_non_interactive(&opts(&dir, tn, false), &ask("resolver", AppRole::Indexer, false)).unwrap();
        assert_eq!((out4["address"].is_null(), out4["key_file"].is_null(), out4["role"].as_str()), (true, true, Some("index")));
        assert_eq!(crate::setup::launch_args(&dir.join(CONFIG_FILE)).unwrap().args, ["--config", CONFIG_FILE]);
        let idx_cfg = IndexerConfig::load(&dir.join(CONFIG_FILE)).unwrap();
        assert!(!idx_cfg.fast_start && !idx_cfg.lean, "index only has no use for either");
        assert_eq!(std::fs::read_to_string(&key_path).unwrap(), secret, "index only leaves the key alone");
    }

    #[test]
    fn an_executor_and_indexer_setup_writes_an_archive_with_an_executor_and_a_key() {
        let d = tempfile::tempdir().unwrap();
        let tn = Some("testnet-10");
        // the key is needed as for the matcher
        let e = setup_non_interactive(&opts(d.path(), tn, true), &ask("resolver", AppRole::ExecutorIndexer, false)).unwrap_err();
        assert!(e.to_string().contains("--new-key"), "{e}");
        assert!(!d.path().join(CONFIG_FILE).exists() && !d.path().join(KEY_FILE).exists());
        let out = setup_non_interactive(&opts(d.path(), tn, false), &ask("resolver", AppRole::ExecutorIndexer, true)).unwrap();
        let key = HotKey::from_file(&d.path().join(KEY_FILE)).unwrap();
        assert_eq!(out["address"], key.address("testnet-10").to_string());
        assert_eq!((out["role"].as_str(), out["roles"].clone()), (Some("run"), serde_json::json!(["executor", "indexer"])));
        let l = crate::setup::launch_args(&d.path().join(CONFIG_FILE)).unwrap();
        assert_eq!(l.args, ["--config", CONFIG_FILE, "--key-file", KEY_FILE, "--pause-file", "kob-data/pause"]);
        let c = IndexerConfig::load(&d.path().join(CONFIG_FILE)).unwrap();
        assert!(!c.lean && c.records.enabled, "full history and the record log");
        assert!(c.fast_start, "the matcher trades within seconds of the first start");
        let text = std::fs::read_to_string(d.path().join(CONFIG_FILE)).unwrap();
        assert!(text.contains("lean = false\n"), "the archive store is written out, not left to the default: {text}");
        // the other roles say so as well
        let m = setup_non_interactive(&opts(d.path(), tn, false), &ask("resolver", AppRole::Executor, false)).unwrap();
        assert_eq!(m["roles"], serde_json::json!(["executor"]));
        assert!(IndexerConfig::load(&d.path().join(CONFIG_FILE)).unwrap().lean);
        let i = setup_non_interactive(&opts(d.path(), tn, false), &ask("resolver", AppRole::Indexer, false)).unwrap();
        assert_eq!((i["roles"].clone(), i["address"].is_null()), (serde_json::json!(["indexer"]), true));
        // the combined role uses the key that is there, like the Executor
        let again = setup_non_interactive(&opts(d.path(), tn, false), &ask("resolver", AppRole::ExecutorIndexer, false)).unwrap();
        assert_eq!(again["address"], out["address"]);
    }

    #[test]
    fn an_executor_without_a_key_or_with_the_wrong_network_is_refused_before_anything_is_written() {
        let d = tempfile::tempdir().unwrap();
        let tn = Some("testnet-10");
        let e = setup_non_interactive(&opts(d.path(), tn, true), &ask("resolver", AppRole::Executor, false)).unwrap_err();
        assert!(e.to_string().contains("--new-key"), "{e}");
        assert!(!d.path().join(CONFIG_FILE).exists() && !d.path().join(KEY_FILE).exists());
        let mut a = ask("resolver", AppRole::Indexer, false);
        a.network = Some("mainnet".into());
        let e = setup_non_interactive(&opts(d.path(), tn, true), &a).unwrap_err();
        assert!(e.to_string().contains("cannot set up mainnet"), "{e}");
        // a build with no deployment network needs --network
        a.network = None;
        assert!(setup_non_interactive(&opts(d.path(), None, true), &a).unwrap_err().to_string().contains("--network"));
        a.network = Some("mainnet".into());
        let out = setup_non_interactive(&opts(d.path(), None, true), &a).unwrap();
        assert_eq!(out["network"], "mainnet");
        // a pinned registry: no token list file
        assert!(!d.path().join(crate::setup::TOKENS_FILE).exists());
        assert!(setup_non_interactive(&opts(d.path(), tn, true), &ask(" ", AppRole::Indexer, false)).is_err());
    }

    #[test]
    fn the_balance_is_read_for_the_configured_key_and_failures_keep_the_address() {
        let d = tempfile::tempdir().unwrap();
        // the balance read resolves the network like every command: a deployment build (CI builds deploy-mainnet) refuses another
        let tn = Some(kob_protocol::artifacts::deployment_network().unwrap_or("testnet-10"));
        let o = setup_non_interactive(&opts(d.path(), tn, false), &ask("resolver", AppRole::Executor, true)).unwrap();
        let v = address_balance(d.path(), &mut Fake(Ok(1_234_500_000))).unwrap();
        assert_eq!(v["address"], o["address"]);
        assert_eq!((v["balance_sompi"].as_u64(), v["balance_kas"].as_str()), (Some(1_234_500_000), Some("12.345")));
        let v = address_balance(d.path(), &mut Fake(Err("transport: refused".into()))).unwrap();
        assert_eq!(v["address"], o["address"]);
        assert!(v["balance_sompi"].is_null() && v["error"].as_str().unwrap().contains("refused"), "{v}");
        let secret = std::fs::read_to_string(d.path().join(KEY_FILE)).unwrap();
        assert!(!v.to_string().contains(secret.trim()));
        // the start banner's address is the same one
        assert_eq!(operator_address(&d.path().join(CONFIG_FILE)).as_deref(), o["address"].as_str());
        // index only: no address
        setup_non_interactive(&opts(d.path(), tn, false), &ask("resolver", AppRole::Indexer, false)).unwrap();
        assert_eq!(operator_address(&d.path().join(CONFIG_FILE)), None);
        assert!(address_balance(d.path(), &mut Fake(Ok(0))).unwrap_err().to_string().contains("no operator key"));
        // no configuration: an error, not a panic
        let e = tempfile::tempdir().unwrap();
        assert!(address_balance(e.path(), &mut Fake(Ok(0))).is_err());
    }
}
