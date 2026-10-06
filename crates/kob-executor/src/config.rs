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
#[serde(default)]
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
#[serde(default)]
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
#[serde(default)]
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
    pub max_ws_connections: usize,
    pub max_ws_per_ip: usize,
    /// A WebSocket session that sends no application message (a subscription change or `{"op":"ping"}`) for this long is
    /// closed; protocol Ping / Pong frames do not count.
    pub ws_idle_timeout_ms: u64,
    /// Messages a WebSocket client may send per second before it is disconnected.
    pub ws_client_msgs_per_sec: u32,
    pub max_ws_subscriptions: usize,
    /// Read connections to the database.
    pub read_pool_size: usize,
    /// Maximum rows in list responses.
    pub max_page_size: usize,
    /// `Access-Control-Allow-Origin` value; unset = no CORS headers (the CDN can add them).
    pub cors_allow_origin: Option<String>,
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
            max_ws_connections: 2_000,
            max_ws_per_ip: 20,
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
#[serde(default)]
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
}

impl Default for NodeConfig {
    fn default() -> Self {
        NodeConfig { url: String::new(), role: NodeRole::Secondary, fetch: true, submit: true, connections: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexerConfig {
    /// Expected network id reported by the node (`testnet-10`, `mainnet`).
    pub network: String,
    /// JSON wRPC endpoint of the node, e.g. `ws://127.0.0.1:18210`: the primary (chain authority) unless `nodes` names
    /// another one.
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
    /// Threads for the pure per-batch work before the single database pass (signature-script parsing, script hashing, placement
    /// verification; `indexer::record::precompute`). 0: the available parallelism, at most 4. See docs/ops/executor.md, *Processing
    /// capacity*.
    pub verify_threads: usize,
    /// Upper bound in MiB of the VSPC windows held in memory ahead of the cursor (fetched, or reserved in flight: a hard bound,
    /// see `indexer::prefetch`). The process holds about three times this at the peak (the parsed windows); 128 by default
    /// (a small host running more than one indexer), 256 or more on a dedicated one.
    pub prefetch_max_mb: u64,
    /// Lag tolerance of the "wait until caught up" gates: the matcher, the keepers and the maintenance jobs of `kob-executor
    /// run` plan while the store is at most this many seconds (`daa_per_second` DAA each) behind the node, whatever the
    /// follower's state (`catching_up` included); beyond it they plan nothing. 0: only while `following` (the strict gate).
    /// `/v1/health` keeps `caught_up` strict and adds `within_lag_tolerance`.
    pub max_lag_secs: u64,
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
    pub daa_per_second: u64,
    /// Lag alarm thresholds in hours (reported by /health and logged).
    pub lag_alarm_hours: Vec<u64>,
    pub tokens_path: Option<PathBuf>,
    pub rules: ListingRules,
    // (a `receipts` table of a v2.4 configuration is ignored: unknown keys are, and the trade receipt is retired)
    #[serde(alias = "rawlog")]
    pub records: RecordsConfig,
    pub api: ApiConfig,
}

impl Default for IndexerConfig {
    fn default() -> Self {
        IndexerConfig {
            network: "testnet-10".to_string(),
            rpc_url: "ws://127.0.0.1:18210".to_string(),
            nodes: Vec::new(),
            primary_fetch: true,
            data_dir: PathBuf::from("kob-index"),
            start: StartMode::PruningPoint,
            poll_interval_ms: 500,
            backoff_ms: 1_000,
            max_backoff_ms: 30_000,
            rpc_timeout_secs: 180,
            fetch_parallel: 4,
            verify_threads: 0,
            prefetch_max_mb: 128,
            max_lag_secs: 30,
            settle_depth_daa: 100,
            max_walkback_blocks: 100_000,
            reorg_window_hours: 12,
            checkpoint_secs: 3600,
            daa_per_second: 10,
            lag_alarm_hours: vec![1, 6, 12, 24],
            tokens_path: None,
            rules: ListingRules::default(),
            records: RecordsConfig::default(),
            api: ApiConfig::default(),
        }
    }
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
        toml::from_str(&s).map_err(|e| ConfigError::Parse(p, e.to_string()))
    }

    /// [`Self::max_lag_secs`] in DAA score.
    pub fn lag_tolerance_daa(&self) -> u64 {
        self.max_lag_secs.saturating_mul(self.daa_per_second)
    }

    /// The primary's endpoint: the `nodes` entry with `role = "primary"`, else `rpc_url`.
    pub fn primary_url(&self) -> &str {
        self.nodes.iter().find(|n| n.role == NodeRole::Primary).map(|n| n.url.as_str()).unwrap_or(&self.rpc_url)
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
}
