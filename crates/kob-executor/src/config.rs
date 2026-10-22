//! Indexer configuration. Loaded from a TOML file (all keys optional) and overridden by CLI flags.

use crate::hex::Hash32;
use crate::tokens::ListingRules;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// Where a fresh database starts following the chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartMode {
    /// The node's current pruning point: replay everything the node still retains. Default.
    PruningPoint,
    /// The node's current sink: no history, only orders created from now on (plus recovery imports).
    Sink,
    /// An explicit chain block hash the node still knows.
    Hash(Hash32),
}

impl std::str::FromStr for StartMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "pruning-point" | "pruning_point" => Ok(StartMode::PruningPoint),
            "sink" => Ok(StartMode::Sink),
            other => {
                Hash32::parse(other).map(StartMode::Hash).map_err(|e| format!("expected pruning-point, sink or a block hash: {e}"))
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecordsConfig {
    /// Keep the permanent record log (default). Without it a lost or outdated database cannot be
    /// rebuilt except by re-reading everything the node still retains.
    pub enabled: bool,
    /// Directory of the log; defaults to `<data_dir>/records`.
    pub dir: Option<PathBuf>,
    /// Segment rotation size in bytes.
    pub segment_bytes: u64,
}

impl Default for RecordsConfig {
    fn default() -> Self {
        RecordsConfig { enabled: true, dir: None, segment_bytes: 256 * 1024 * 1024 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RateLimitConfig {
    /// Sustained requests per second per client (0 disables rate limiting).
    pub per_ip_rps: f64,
    /// Burst size per client.
    pub per_ip_burst: u32,
    /// Sustained requests per second across all clients (0 = unlimited).
    pub global_rps: f64,
    pub global_burst: u32,
    /// Idle time after which a client bucket is forgotten.
    pub bucket_ttl_secs: u64,
    /// Hard cap on tracked clients (memory bound against address-spray floods).
    pub max_tracked_clients: usize,
    /// IPv6 clients are limited per this many leading address bits (64: one subscriber; 48 groups a site, against an attacker
    /// that rotates addresses through a larger prefix).
    pub ipv6_prefix_bits: u8,
    /// IPv6 clients are ALSO limited together per this wider prefix (48: a site), so an attacker rotating through the
    /// /64s of one allocation gets one shared budget instead of a fresh one per /64; 0 disables the site bucket.
    pub ipv6_site_prefix_bits: u8,
    /// Sustained requests per second of one IPv6 site (`ipv6_site_prefix_bits`).
    pub per_site_rps: f64,
    /// Burst size of one IPv6 site.
    pub per_site_burst: u32,
    /// Bucket tokens one heavy read (candles, stats, depth, trades, books, strays, holdings) costs; a light request costs 1.
    pub heavy_route_cost: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        RateLimitConfig {
            per_ip_rps: 20.0,
            per_ip_burst: 60,
            global_rps: 500.0,
            global_burst: 1000,
            bucket_ttl_secs: 600,
            max_tracked_clients: 100_000,
            ipv6_prefix_bits: 64,
            ipv6_site_prefix_bits: 48,
            per_site_rps: 100.0,
            per_site_burst: 300,
            heavy_route_cost: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApiConfig {
    pub enabled: bool,
    pub listen: SocketAddr,
    pub rate_limit: RateLimitConfig,
    /// Peers (CIDR or bare IP) allowed to set the forwarded-client header, i.e. the reverse proxy or CDN.
    /// Requests from any other peer are keyed by their socket address and the header is ignored.
    pub trusted_proxies: Vec<String>,
    /// Header carrying the client address behind a trusted proxy (`x-forwarded-for` takes the
    /// right-most address that is not itself a trusted proxy; any other name is read as a single address).
    pub client_ip_header: String,
    /// Concurrent in-flight REST requests.
    pub max_concurrent_requests: usize,
    pub request_timeout_ms: u64,
    /// A request head must arrive within this (also the keep-alive idle limit between requests): slow-header connections
    /// are closed before the guard ever sees them.
    pub header_timeout_ms: u64,
    /// A response write that makes no progress for this long (the client stopped reading) closes the connection; 0 = off.
    pub write_timeout_ms: u64,
    /// Open HTTP connections at once (an upgraded WebSocket counts against `max_ws_connections` instead).
    pub max_connections: usize,
    /// Open HTTP connections per socket peer (IPv6 per `rate_limit.ipv6_prefix_bits` prefix; trusted proxies exempt); 0 = no cap.
    pub max_connections_per_ip: usize,
    /// Open HTTP connections per IPv6 site (`rate_limit.ipv6_site_prefix_bits`, all its /64s together; trusted proxies
    /// exempt); 0 = no site cap.
    pub max_connections_per_site: usize,
    pub max_ws_connections: usize,
    pub max_ws_per_ip: usize,
    /// Open WebSocket connections per IPv6 site (`rate_limit.ipv6_site_prefix_bits`, all its /64s together); 0 = no site cap.
    pub max_ws_per_site: usize,
    /// A WebSocket session that sends no application message (a subscription change or `{"op":"ping"}`) for this long is
    /// closed; protocol Ping / Pong frames do not count.
    pub ws_idle_timeout_ms: u64,
    /// Messages a WebSocket client may send per second before it is disconnected.
    pub ws_client_msgs_per_sec: u32,
    pub max_ws_subscriptions: usize,
    /// Read connections to the database (plus one of the light lane, `db::ReadPool::with_light`).
    pub read_pool_size: usize,
    /// Maximum rows in list responses.
    pub max_page_size: usize,
    /// `Access-Control-Allow-Origin` value; unset = no CORS headers (the CDN can add them).
    pub cors_allow_origin: Option<String>,
}

impl ApiConfig {
    /// A warning for a listener other hosts can reach with no trusted proxy: a reverse proxy or CDN in front of it makes every
    /// client arrive from the proxy's address, so all of them share one set of per-client limits (rate, connections,
    /// WebSockets) until `trusted_proxies` names the proxy. `None` for a loopback listener (the default, and what
    /// `kob-executor setup` writes): only this host's own programs reach it, a person's browser and tools, for whom one shared
    /// set of limits is right; an operator who puts a proxy in front of it on the same host lists it as the guide says
    /// (docs/ops/executor.md, Part B, 4). `None` with the API off or a trusted proxy configured.
    pub fn proxy_warning(&self) -> Option<String> {
        (self.enabled && !self.listen.ip().is_loopback() && self.trusted_proxies.is_empty()).then(|| {
            format!(
                "the read API listens on {}, reachable from other hosts, with no trusted_proxies: behind a reverse proxy or CDN \
                 every client shares the per-client limits of the proxy's address; list its addresses in api.trusted_proxies \
                 (docs/ops/executor.md, Part B, 4)",
                self.listen
            )
        })
    }
}

impl Default for ApiConfig {
    fn default() -> Self {
        ApiConfig {
            enabled: true,
            listen: "127.0.0.1:8090".parse().expect("static address"),
            rate_limit: RateLimitConfig::default(),
            trusted_proxies: Vec::new(),
            client_ip_header: "x-forwarded-for".to_string(),
            max_concurrent_requests: 256,
            request_timeout_ms: 10_000,
            header_timeout_ms: 10_000,
            write_timeout_ms: 30_000,
            max_connections: 4_096,
            max_connections_per_ip: 64,
            max_connections_per_site: 512,
            max_ws_connections: 2_000,
            max_ws_per_ip: 20,
            max_ws_per_site: 200,
            ws_idle_timeout_ms: 60_000,
            ws_client_msgs_per_sec: 20,
            max_ws_subscriptions: 64,
            read_pool_size: 4,
            max_page_size: 200,
            cors_allow_origin: None,
        }
    }
}

/// What a node of `nodes` is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NodeRole {
    /// The authority for the selected chain (chain hashes, acceptance data, reorgs); replaces `rpc_url`.
    Primary,
    /// Serves windows of transaction bodies (checked against the primary) and takes submissions.
    #[default]
    Secondary,
}

/// One more node (docs/ops/executor.md, Part B 3, *Several nodes*).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NodeConfig {
    /// JSON wRPC endpoint (`ws://` or `wss://`).
    pub url: String,
    pub role: NodeRole,
    /// Windows of the parallel fetch go to it too.
    pub fetch: bool,
    /// The executor's transactions are submitted to it too.
    pub submit: bool,
    /// Windows fetched from it at once (default: `fetch_parallel`).
    pub connections: Option<usize>,
    /// Its Borsh wRPC endpoint when `borsh` is on (default: derived from `url`, `rpc::borsh::borsh_url_for`).
    pub borsh_url: Option<String>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        NodeConfig { url: String::new(), role: NodeRole::Secondary, fetch: true, submit: true, connections: None, borsh_url: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndexerConfig {
    /// Expected network id reported by the node (`testnet-10`, `mainnet`). Empty (the default): the build's deployment network
    /// ([`resolve_network`]); the reference build requires it.
    pub network: String,
    /// JSON wRPC endpoint of the node: the primary (chain authority) unless `nodes` names another one. Empty (the default):
    /// the local node's default JSON wRPC port of the network ([`default_wrpc_url`]: `ws://127.0.0.1:18110` on mainnet,
    /// `ws://127.0.0.1:18210` on testnet-10).
    pub rpc_url: String,
    /// Further nodes: windows of transaction bodies are fetched from all of them in parallel (checked against the primary)
    /// and transactions submitted to all of them. Empty: the one node of `rpc_url`.
    pub nodes: Vec<NodeConfig>,
    /// With other nodes: the primary serves windows of the parallel fetch too (`false`: only when no other node can, e.g. a
    /// primary much slower than the others; it stays the chain authority either way).
    pub primary_fetch: bool,
    pub data_dir: PathBuf,
    pub start: StartMode,
    /// Pause between polls once the cursor is at the node's sink.
    pub poll_interval_ms: u64,
    /// First delay after a failed call; doubles up to `max_backoff_ms`.
    pub backoff_ms: u64,
    pub max_backoff_ms: u64,
    /// Per-request timeout for the node in seconds (VSPC batches are large).
    pub rpc_timeout_secs: u64,
    /// While far behind the node, the follower fetches this many VSPC windows at once, one connection each, and
    /// applies them in chain order (1: one batch at a time). See docs/ops/executor.md, Part B 3, *Parallel fetch*.
    pub fetch_parallel: usize,
    /// Fetch the windows of transaction bodies and the primary's accepted transaction ids over each node's Borsh wRPC endpoint
    /// (the JSON URL's `/json` path as `/borsh`, or its default port `18xxx` as `17xxx`; a node's `borsh_url` overrides):
    /// about a third of the JSON bytes. Every other call stays JSON. See docs/ops/executor.md, *Borsh windows*.
    pub borsh: bool,
    /// Threads for the pure per-batch work before the single database pass (signature-script parsing, script hashing, placement
    /// verification; `indexer::record::precompute`). 0: the available parallelism, at most 4. See docs/ops/executor.md, *Processing
    /// capacity*.
    pub verify_threads: usize,
    /// Upper bound in MiB of the VSPC windows held in memory ahead of the cursor (fetched, or reserved in flight: a hard bound,
    /// see `indexer::prefetch`). The process holds about three times this at the peak (the parsed windows); 128 by default
    /// (a small host running more than one indexer), 256 or more on a dedicated one.
    pub prefetch_max_mb: u64,
    /// The parallel fetch runs while the cursor is more than this many blue score (about one block each at 10 BPS) behind the
    /// node's sink; closer, the follower takes single steps on one connection. 1,200 by default (two minutes). Under a flood
    /// that one connection cannot carry (one step slower than the chain it covers), the lag settles near this value: lower it
    /// (e.g. 30) to keep the windows parallel up to the tip. See docs/ops/executor.md, *Parallel fetch*.
    pub prefetch_min_lag_blue: u64,
    /// Lag tolerance of the "wait until caught up" gates: the matcher, the keepers and the maintenance jobs of `kob-executor
    /// run` plan while the store is at most this many seconds (`daa_per_second` DAA each) behind the node, whatever the
    /// follower's state (`catching_up` included); beyond it they plan nothing. 0: only while `following` (the strict gate).
    /// `/v1/health` keeps `caught_up` strict and adds `within_lag_tolerance`.
    pub max_lag_secs: u64,
    /// `kob-executor run` asks the node which listed orders it still holds (and leaves out those it no longer does) only while
    /// the store is at least this many seconds behind the node: the query is a node round trip per tick, which loses races to
    /// other executors when the book is current anyway. 0: ask every tick.
    pub verify_live_orders_min_lag_secs: u64,
    /// A transaction counts as settled once its chain block is this many DAA behind the node's virtual DAA.
    /// Default 100 (about 10 s at 10 BPS), the release plan's "accepted plus N DAA".
    pub settle_depth_daa: u64,
    /// How many stored chain blocks the follower may probe, newest first, when the node does not know
    /// the cursor (only blocks of the reorg window are stored).
    pub max_walkback_blocks: u64,
    /// Chain blocks are kept for this many hours behind the cursor (the reorg / finality window; 12 h
    /// is consensus finality) and deleted after that.
    pub reorg_window_hours: u64,
    /// An otherwise empty batch is still written to the record log when the cursor moved this many
    /// seconds since the last logged cursor, so a rebuild resumes close to the tip.
    pub checkpoint_secs: u64,
    /// A KCC-23 candidate preparation no genesis has spent is kept this many hours (`indexer::kcc23`): a token's genesis must be
    /// accepted within it of its preparation for the indexer to serve the declaration. 72 by default.
    pub declaration_window_hours: u64,
    pub daa_per_second: u64,
    /// Lag alarm thresholds in hours (reported by /health and logged).
    pub lag_alarm_hours: Vec<u64>,
    pub tokens_path: Option<PathBuf>,
    pub rules: ListingRules,
    #[serde(alias = "rawlog")]
    pub records: RecordsConfig,
    pub api: ApiConfig,
    /// What the binary started without arguments runs with this file (`crate::setup`; `kob-executor setup` writes it into
    /// `kob-executor.toml` next to the executable). Every command given the file as `--config` ignores the table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch: Option<LaunchConfig>,
    /// The `[receipts]` table of a v2.4 configuration (the trade receipt is retired): accepted and ignored. Every other
    /// unknown key is refused, so a misspelt key is never dropped silently.
    #[serde(skip_serializing)]
    pub receipts: Option<toml::Value>,
}

impl Default for IndexerConfig {
    fn default() -> Self {
        IndexerConfig {
            network: String::new(),
            rpc_url: String::new(),
            nodes: Vec::new(),
            primary_fetch: true,
            data_dir: PathBuf::from("kob-index"),
            start: StartMode::PruningPoint,
            poll_interval_ms: 500,
            backoff_ms: 1_000,
            max_backoff_ms: 30_000,
            rpc_timeout_secs: 180,
            fetch_parallel: 4,
            borsh: false,
            verify_threads: 0,
            prefetch_max_mb: 128,
            prefetch_min_lag_blue: 1_200,
            max_lag_secs: 30,
            verify_live_orders_min_lag_secs: 10,
            settle_depth_daa: 100,
            max_walkback_blocks: 100_000,
            reorg_window_hours: 12,
            checkpoint_secs: 3600,
            declaration_window_hours: 72,
            daa_per_second: 10,
            lag_alarm_hours: vec![1, 6, 12, 24],
            tokens_path: None,
            rules: ListingRules::default(),
            records: RecordsConfig::default(),
            api: ApiConfig::default(),
            launch: None,
            receipts: None,
        }
    }
}

/// The role a start without arguments runs (`[launch] role`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LaunchRole {
    /// `kob-executor run`: the indexer, the matcher and the keepers (needs the operator's key).
    #[default]
    Run,
    /// `kob-executor index`: the indexer and its read API alone (no key, no funds).
    Index,
}

impl LaunchRole {
    /// The subcommand of the role.
    pub fn subcommand(self) -> &'static str {
        match self {
            LaunchRole::Run => "run",
            LaunchRole::Index => "index",
        }
    }
}

/// The `[launch]` table: the command-line options of a start without arguments that are not indexer keys. A start without
/// arguments runs `kob-executor <role> --config <this file> --key-file ... --pause-file ... --x402-config ... <args>` in the
/// file's directory, so its relative paths (these and `data_dir`, `tokens_path`) are relative to the file
/// (`crate::setup::launch_args`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LaunchConfig {
    pub role: LaunchRole,
    /// The operator's key file (`--key-file`; `run` only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_file: Option<PathBuf>,
    /// The kill switch (`--pause-file`; `run` only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pause_file: Option<PathBuf>,
    /// The x402 facilitator's configuration (`--x402-config`; `run` only, off when absent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x402_config: Option<PathBuf>,
    /// Further options of the role's subcommand, appended as given (e.g. `["--sweep-own-strays"]`).
    pub args: Vec<String>,
}

/// The network a command serves: the one given (`--network`, `network = ...`), else the build's deployment network
/// (`kob_protocol::artifacts::deployment_network`: `testnet-10` with the feature `deploy-tn10`, `mainnet` with
/// `deploy-mainnet`). The reference build has none, so it requires the network explicitly; a deployment build refuses
/// another network. Never an implicit testnet-10.
pub fn resolve_network(given: Option<&str>) -> Result<String, String> {
    resolve_network_for(given, kob_protocol::artifacts::deployment_network())
}

/// [`resolve_network`] for a build whose deployment network is `build`.
pub fn resolve_network_for(given: Option<&str>, build: Option<&str>) -> Result<String, String> {
    match (given.map(str::trim).filter(|s| !s.is_empty()), build) {
        (Some(n), Some(b)) if n != b => Err(format!("this binary is the {b} deployment build but the configured network is {n}")),
        (Some(n), _) => Ok(n.to_string()),
        (None, Some(b)) => Ok(b.to_string()),
        (None, None) => Err("no network configured: pass --network mainnet or --network testnet-10 (or `network = \"...\"` in \
                             the configuration file); only a deployment build (--features deploy-mainnet or deploy-tn10) \
                             implies its network"
            .to_string()),
    }
}

/// The local node's JSON wRPC endpoint of a network: rusty-kaspa v2.1.0 `NetworkType::default_json_rpc_port`
/// (`consensus/core/src/network.rs`: mainnet 18110, testnet 18210; the Borsh ports are 17110 / 17210). `None` for any
/// other network (pass the endpoint explicitly).
pub fn default_wrpc_url(network: &str) -> Option<String> {
    let port = match network {
        "mainnet" => 18110,
        "testnet-10" => 18210,
        _ => return None,
    };
    Some(format!("ws://127.0.0.1:{port}"))
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read config {0}: {1}")]
    Io(String, std::io::Error),
    #[error("invalid config {0}: {1}")]
    Parse(String, String),
}

impl IndexerConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let p = path.display().to_string();
        let s = std::fs::read_to_string(path).map_err(|e| ConfigError::Io(p.clone(), e))?;
        let cfg: IndexerConfig = toml::from_str(&s).map_err(|e| ConfigError::Parse(p, e.to_string()))?;
        if cfg.receipts.is_some() {
            tracing::warn!("config: the [receipts] table is retired and ignored");
        }
        Ok(cfg)
    }

    /// [`Self::verify_live_orders_min_lag_secs`] in DAA score.
    pub fn verify_live_min_lag_daa(&self) -> u64 {
        self.verify_live_orders_min_lag_secs.saturating_mul(self.daa_per_second)
    }

    /// [`Self::max_lag_secs`] in DAA score.
    pub fn lag_tolerance_daa(&self) -> u64 {
        self.max_lag_secs.saturating_mul(self.daa_per_second)
    }

    /// The primary's endpoint: the `nodes` entry with `role = "primary"`, else `rpc_url`.
    pub fn primary_url(&self) -> &str {
        self.nodes.iter().find(|n| n.role == NodeRole::Primary).map(|n| n.url.as_str()).unwrap_or(&self.rpc_url)
    }

    /// The Borsh endpoint used for the node at `url`, if `borsh` is on: its configured `borsh_url`, else the one derived from
    /// `url` (`None`: that node stays JSON).
    pub fn borsh_url_of(&self, url: &str) -> Option<String> {
        if !self.borsh {
            return None;
        }
        let configured = self.nodes.iter().find(|n| n.url == url).and_then(|n| n.borsh_url.clone());
        configured.or_else(|| crate::rpc::borsh::borsh_url_for(url))
    }

    /// The other nodes, in order (an entry repeating the primary's URL is skipped).
    pub fn secondaries(&self) -> Vec<&NodeConfig> {
        let primary = self.primary_url();
        self.nodes.iter().filter(|n| n.role == NodeRole::Secondary && n.url != primary).collect()
    }

    /// Windows of the parallel fetch in flight at once: `fetch_parallel` on the primary plus each fetching secondary's
    /// connections (1 turns the parallel fetch off, and with it the other nodes' windows).
    pub fn fetch_windows(&self) -> usize {
        let base = self.fetch_parallel.max(1);
        if base == 1 {
            return 1;
        }
        let others = self.secondaries().iter().filter(|n| n.fetch).map(|n| n.connections.unwrap_or(base).max(1)).sum::<usize>();
        if self.primary_fetches() {
            base + others
        } else {
            others
        }
    }

    /// The primary serves windows of the parallel fetch (`primary_fetch`, or no other node does).
    pub fn primary_fetches(&self) -> bool {
        self.primary_fetch || !self.secondaries().iter().any(|n| n.fetch)
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("index.sqlite3")
    }

    pub fn records_dir(&self) -> PathBuf {
        self.records.dir.clone().unwrap_or_else(|| self.data_dir.join("records"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_network_is_explicit_or_the_deployment_builds() {
        // the reference build: no implicit network
        let e = resolve_network_for(None, None).unwrap_err();
        assert!(e.contains("--network mainnet or --network testnet-10"), "{e}");
        assert_eq!(resolve_network_for(Some("  "), None), Err(e));
        assert_eq!(resolve_network_for(Some("mainnet"), None).unwrap(), "mainnet");
        assert_eq!(resolve_network_for(Some("testnet-10"), None).unwrap(), "testnet-10");
        // a deployment build implies its network and refuses another one
        assert_eq!(resolve_network_for(None, Some("mainnet")).unwrap(), "mainnet");
        assert_eq!(resolve_network_for(None, Some("testnet-10")).unwrap(), "testnet-10");
        assert_eq!(resolve_network_for(Some("mainnet"), Some("mainnet")).unwrap(), "mainnet");
        let e = resolve_network_for(Some("testnet-10"), Some("mainnet")).unwrap_err();
        assert!(e.contains("mainnet deployment build") && e.contains("testnet-10"), "{e}");
        // this build
        assert_eq!(resolve_network(None).ok().as_deref(), kob_protocol::artifacts::deployment_network());
        // the node endpoint follows the network (rusty-kaspa's default JSON wRPC ports), never TN10's on mainnet
        assert_eq!(default_wrpc_url("mainnet").as_deref(), Some("ws://127.0.0.1:18110"));
        assert_eq!(default_wrpc_url("testnet-10").as_deref(), Some("ws://127.0.0.1:18210"));
        assert_eq!(default_wrpc_url("devnet"), None);
        // the configuration's defaults name no network and no node
        let d = IndexerConfig::default();
        assert_eq!((d.network.as_str(), d.rpc_url.as_str()), ("", ""));
        let t: IndexerConfig = toml::from_str("data_dir = \"x\"").unwrap();
        assert_eq!((t.network.as_str(), t.rpc_url.as_str()), ("", ""));
    }

    #[test]
    fn partial_toml_uses_defaults() {
        let c: IndexerConfig =
            toml::from_str("network = \"mainnet\"\n[api]\nlisten = \"0.0.0.0:9000\"\ntrusted_proxies = [\"10.0.0.0/8\"]\n").unwrap();
        assert_eq!(c.network, "mainnet");
        assert_eq!(c.api.listen.port(), 9000);
        assert_eq!(c.api.trusted_proxies, vec!["10.0.0.0/8".to_string()]);
        assert_eq!(c.max_walkback_blocks, 100_000);
        assert!(c.records.enabled);
    }

    #[test]
    fn verify_live_orders_lag_defaults_to_ten_seconds() {
        let d: IndexerConfig = toml::from_str("").unwrap();
        assert_eq!((d.verify_live_orders_min_lag_secs, d.verify_live_min_lag_daa()), (10, 100));
        let always: IndexerConfig = toml::from_str("verify_live_orders_min_lag_secs = 0").unwrap();
        assert_eq!(always.verify_live_min_lag_daa(), 0);
    }

    #[test]
    fn nodes_parse_with_defaults() {
        let c: IndexerConfig = toml::from_str(
            "rpc_url = \"ws://a:1\"
fetch_parallel = 4
[[nodes]]
url = \"wss://b/json\"
[[nodes]]
url = \"wss://c/json\"
connections = 2
submit = false
",
        )
        .unwrap();
        assert_eq!(c.primary_url(), "ws://a:1");
        let s = c.secondaries();
        assert_eq!(s.len(), 2);
        assert!(s[0].fetch && s[0].submit && s[0].connections.is_none());
        assert!(s[1].fetch && !s[1].submit && s[1].connections == Some(2));
        assert_eq!(c.fetch_windows(), 4 + 4 + 2);
        let p: IndexerConfig = toml::from_str(
            "rpc_url = \"ws://a:1\"
[[nodes]]
url = \"ws://p:2\"
role = \"primary\"
[[nodes]]
url = \"ws://a:1\"
",
        )
        .unwrap();
        assert_eq!(p.primary_url(), "ws://p:2");
        assert_eq!(p.secondaries().len(), 1, "rpc_url listed as a secondary is one");
        let off: IndexerConfig = toml::from_str(
            "fetch_parallel = 1
[[nodes]]
url = \"wss://b\"
",
        )
        .unwrap();
        assert_eq!(off.fetch_windows(), 1);
        let mut np = c.clone();
        np.primary_fetch = false;
        assert!(!np.primary_fetches());
        assert_eq!(np.fetch_windows(), 4 + 2);
        np.nodes.clear();
        assert!(np.primary_fetches(), "no other node: the primary fetches whatever the setting");
    }

    #[test]
    fn start_mode_parses() {
        assert_eq!("sink".parse::<StartMode>().unwrap(), StartMode::Sink);
        assert_eq!("pruning-point".parse::<StartMode>().unwrap(), StartMode::PruningPoint);
        let h = Hash32([3; 32]);
        assert_eq!(h.to_hex().parse::<StartMode>().unwrap(), StartMode::Hash(h));
        assert!("nonsense".parse::<StartMode>().is_err());
    }

    #[test]
    fn unknown_keys_are_refused_at_every_level() {
        for (doc, key) in [
            ("[api]\nlisten = \"127.0.0.1:8090\"\ntrusted_proxy = [\"127.0.0.1\"]\n", "trusted_proxy"),
            ("rpc_urls = \"ws://a:1\"\n", "rpc_urls"),
            ("[api.rate_limit]\nper_ip_rp = 5.0\n", "per_ip_rp"),
            ("[rules]\nmin_order_value = 1\n", "min_order_value"),
            ("[records]\nsegment_size = 1\n", "segment_size"),
            ("[[nodes]]\nurl = \"ws://b:1\"\nrole = \"secondary\"\nfetches = true\n", "fetches"),
        ] {
            let e = toml::from_str::<IndexerConfig>(doc).err().unwrap_or_else(|| panic!("accepted: {doc}"));
            assert!(e.to_string().contains(key), "{e}");
        }
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("index.toml");
        std::fs::write(&p, "[api]\ntrusted_proxy = [\"127.0.0.1\"]\n").unwrap();
        assert!(matches!(IndexerConfig::load(&p), Err(ConfigError::Parse(..))));
    }

    #[test]
    fn retired_tables_are_still_accepted() {
        let c: IndexerConfig =
            toml::from_str("[receipts]\nenabled = true\nanything = 1\n[rawlog]\nenabled = false\n").expect("retired keys");
        assert!(c.receipts.is_some());
        assert!(!c.records.enabled, "[rawlog] is [records]");
        assert!(!toml::to_string(&c).unwrap().contains("receipts"));
    }

    #[test]
    fn only_a_listener_other_hosts_reach_is_warned_about_without_a_trusted_proxy() {
        // loopback (the default and the setup's): only this host's programs reach it, no warning
        let mut a = ApiConfig::default();
        assert_eq!(a.proxy_warning(), None);
        a.listen = "[::1]:8090".parse().unwrap();
        assert_eq!(a.proxy_warning(), None);
        // reachable from other hosts: warned until a trusted proxy is named
        for listen in ["0.0.0.0:8090", "[::]:8090", "192.168.1.10:8090", "203.0.113.5:443"] {
            a.listen = listen.parse().unwrap();
            let w = a.proxy_warning().unwrap_or_else(|| panic!("{listen}: no warning"));
            assert!(w.contains(listen) && w.contains("api.trusted_proxies"), "{w}");
        }
        a.trusted_proxies = vec!["10.0.0.0/8".into()];
        assert_eq!(a.proxy_warning(), None);
        a.trusted_proxies.clear();
        a.enabled = false;
        assert_eq!(a.proxy_warning(), None);
    }

    /// The example configuration of docs/ops/executor.md (Part B, 4) parses, and its API sits behind a proxy it trusts.
    #[test]
    fn the_documented_example_configuration_parses() {
        let doc = include_str!("../../../docs/ops/executor.md");
        let start = doc.find("### 4. Running it").expect("section");
        let body = &doc[start..];
        let open = body.find("```toml\n").expect("toml block") + "```toml\n".len();
        let close = open + body[open..].find("```").expect("end");
        let c: IndexerConfig = toml::from_str(&body[open..close]).expect("the example parses");
        assert!(c.api.listen.ip().is_loopback());
        assert_eq!(c.api.trusted_proxies, vec!["127.0.0.1".to_string()]);
        assert_eq!(c.api.proxy_warning(), None);
    }
}
