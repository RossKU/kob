//! Facilitator configuration: a strict JSON file (unknown fields are rejected) plus CLI overrides.
//!
//! ```json
//! {
//!   "network": "kaspa:testnet-10",
//!   "node": "ws://127.0.0.1:18210",
//!   "listen": "127.0.0.1:8402",
//!   "ledger": "x402-ledger.jsonl",
//!   "auth": "required",
//!   "confirmationsDaa": 100,
//!   "maxFeeSompi": 50000000,
//!   "minAmountSompi": 1000,
//!   "swap": true,
//!   "registry": "builtin",
//!   "tokens": [ { "covenantId": "<64 hex>", "programTemplateHash": "<64 hex>", "extensionCommitment": "<64 hex>",
//!                 "custody": "issuer-controlled", "ticker": "USDX", "decimals": 6 } ],
//!   "allowIssuerControlled": false,
//!   "merchants": [ { "id": "shop-1", "apiKeySha256": "<64 hex sha256 of the API key>",
//!                    "allowedPayTo": ["kaspatest:..."], "allowedAssets": ["KAS"],
//!                    "rateLimit": { "burst": 20, "perSecond": 5 } } ],
//!   "adminKeySha256": "<64 hex>",
//!   "rateLimit": { "perIp": { "burst": 30, "perSecond": 10 }, "perSite": { "burst": 150, "perSecond": 50 },
//!                  "anonymous": { "burst": 1000, "perSecond": 300 }, "perMerchant": { "burst": 60, "perSecond": 20 } },
//!   "maxBodyBytes": 1048576,
//!   "bodyDeadlineMs": 10000,
//!   "headerTimeoutMs": 10000,
//!   "settleWaitMs": 30000,
//!   "pollIntervalMs": 200,
//!   "submitRetries": 2,
//!   "rebroadcasts": 2,
//!   "nodeTimeoutMs": 5000,
//!   "reorgWatchDaa": 36000,
//!   "reconcileIntervalSeconds": 30,
//!   "maxConcurrentSettles": 64,
//!   "maxConcurrentInvoicePays": 16,
//!   "maxConnections": 1024,
//!   "maxConnectionsPerIp": 64,
//!   "maxConnectionsPerSite": 128,
//!   "ipv6PrefixBits": 64,
//!   "ipv6SitePrefixBits": 48,
//!   "killSwitchFile": "x402.kill",
//!   "intents": { "enabled": true, "keeperPubkey": "<64 hex x-only key>", "fillerSompi": 20000000,
//!                "maxAttempts": 20, "maxBuilds": 24, "maxCandidates": 8, "lockMarginDaa": 10 },
//!   "invoices": { "enabled": true, "store": "x402-invoices.jsonl", "maxLifetimeSeconds": 604800,
//!                 "publicUrl": "https://pay.example.com", "maxOpenPerMerchant": 10000 }
//! }
//! ```
//!
//! `intents` (intent-based swap-and-pay, `kob-intent-v1`) needs the book: it is served only inside
//! `kob-executor run` (`--x402-config`), where the indexer's book is at hand; the stand-alone `kob-executor x402`
//! refuses it. `invoices` works in both.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use kaspa_addresses::Address;
use kob_protocol::artifacts::token_template_by_hash;
use kob_protocol::registry::{parse_hex32, Family, Registry};
use kob_x402::policy::{AllowedToken, Custody, Policy, TokenAllowlist};
use kob_x402::wire::{Finality, Network};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::ratelimit::Rate;

/// Configuration failures.
#[derive(Debug, thiserror::Error)]
#[error("x402 config: {0}")]
pub struct ConfigError(pub String);

fn err<T>(m: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError(m.into()))
}

/// Authentication mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    /// Every `/verify` and `/settle` needs a merchant API key (default).
    Required,
    /// No authentication, for a local operator only: accepted only when listening on a loopback address, without
    /// `trustedProxies` and with `openAuthNoProxy: true` (nothing on the host relays to `listen`); a request from another host or
    /// with a forwarding header is refused. A same-host relay that adds no header cannot be detected: never use one with it.
    Open,
}

/// One token bucket.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RateCfg {
    pub burst: u32,
    pub per_second: f64,
}

impl RateCfg {
    pub fn rate(self) -> Rate {
        Rate::new(self.burst, self.per_second)
    }
}

/// Request limits. A request without a merchant key takes a token of its client (`perIp`; IPv6 per `ipv6PrefixBits`), of its
/// IPv6 site (`perSite`; all /64s of one `ipv6SitePrefixBits` prefix together) and of the one bucket all such requests share
/// (`anonymous`), in this order (a client that is already limited takes nothing from the shared buckets); a request with a
/// merchant key takes a token of its merchant (`perMerchant` or the merchant's own `rateLimit`).
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase", default)]
pub struct RateLimits {
    pub per_ip: RateCfg,
    pub per_site: RateCfg,
    pub anonymous: RateCfg,
    pub per_merchant: RateCfg,
}

impl Default for RateLimits {
    fn default() -> Self {
        RateLimits {
            per_ip: RateCfg { burst: 30, per_second: 10.0 },
            per_site: RateCfg { burst: 150, per_second: 50.0 },
            anonymous: RateCfg { burst: 1_000, per_second: 300.0 },
            per_merchant: RateCfg { burst: 60, per_second: 20.0 },
        }
    }
}

/// A merchant (resource server) allowed to call the facilitator.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MerchantCfg {
    pub id: String,
    /// Lowercase hex SHA-256 of the merchant's API key (the key itself is never stored).
    pub api_key_sha256: String,
    /// `payTo` addresses this merchant may request payment to.
    pub allowed_pay_to: Vec<String>,
    /// Assets (`KAS` or KCC-20 covenant ids) this merchant may request.
    pub allowed_assets: Vec<String>,
    #[serde(default)]
    pub rate_limit: Option<RateCfg>,
}

/// An operator-listed token (added to the registry-derived allowlist).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TokenCfg {
    pub covenant_id: String,
    /// Template hash of the pinned token program of either family (must be a program built into this
    /// binary: the KCC-20 programs, KaspaCom's included, and the KRON programs).
    pub program_template_hash: String,
    /// `kcc20` (default) or `kron`; must be the family of the program. A KRON token is only accepted
    /// as a swap-and-pay pay asset.
    #[serde(default)]
    pub family: Option<String>,
    /// 64 lowercase hex; a KRON token has none (omit it or give all zeros).
    #[serde(default)]
    pub extension_commitment: String,
    /// `unconditional` or `issuer-controlled`.
    pub custody: String,
    #[serde(default)]
    pub ticker: String,
    #[serde(default)]
    pub decimals: u8,
}

/// Intent-based swap-and-pay: the facilitator executes payers' router intents as their keeper.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase", default)]
pub struct IntentCfg {
    pub enabled: bool,
    /// x-only key (64 hex) receiving what token intents leave to the keeper (spread, the intent's unused KAS) and their
    /// filler outputs. The facilitator never signs with it.
    pub keeper_pubkey: String,
    /// Value of an output that only holds an index open (paid to the payer for KasToToken, to the keeper otherwise).
    pub filler_sompi: u64,
    /// Executions tried per intent before it is given up.
    pub max_attempts: usize,
    /// Candidate executions built per planning pass.
    pub max_builds: usize,
    /// Orders considered per side (best price first).
    pub max_candidates: usize,
    /// An execution proves the virtual DAA score minus this margin (CLTV).
    pub lock_margin_daa: u64,
}

impl Default for IntentCfg {
    fn default() -> Self {
        IntentCfg {
            enabled: false,
            keeper_pubkey: String::new(),
            filler_sompi: 20_000_000,
            max_attempts: 20,
            max_builds: 24,
            max_candidates: 8,
            lock_margin_daa: 10,
        }
    }
}

/// Invoices (`/invoices`).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase", default)]
pub struct InvoiceCfg {
    pub enabled: bool,
    /// Store path (`:memory:` = volatile; relative to the configuration file's directory); default: the ledger path with
    /// `.invoices.jsonl` appended.
    pub store: Option<String>,
    /// Longest invoice lifetime accepted at registration.
    pub max_lifetime_seconds: u64,
    /// Public base URL of this facilitator (`https://pay.example.com`): registration answers `<publicUrl>/invoices/<id>`.
    pub public_url: Option<String>,
    /// Most unexpired invoices per merchant.
    pub max_open_per_merchant: usize,
    /// Most refused payments (duplicate or late; one per spent output) kept as evidence per invoice.
    pub max_extra_payments_per_invoice: usize,
}

impl Default for InvoiceCfg {
    fn default() -> Self {
        InvoiceCfg {
            enabled: false,
            store: None,
            max_lifetime_seconds: 7 * 24 * 3600,
            public_url: None,
            max_open_per_merchant: 10_000,
            max_extra_payments_per_invoice: 16,
        }
    }
}

/// The facilitator configuration file.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase", default)]
pub struct X402Config {
    pub network: String,
    pub node: String,
    pub listen: String,
    /// The replay ledger (`:memory:` = volatile). A relative path is relative to the configuration file's directory
    /// ([`X402Config::load`]), as are `invoices.store`, `registry` and `killSwitchFile`.
    pub ledger: String,
    pub auth: AuthMode,
    /// Required with `auth: "open"`: the operator asserts that nothing on this host relays connections to `listen` (no
    /// reverse proxy, TLS terminator, port forwarder or tunnel). Such a relay connects from the loopback address and need not
    /// add any header, so the facilitator cannot tell it from a local caller; with one, use `auth: "required"`.
    pub open_auth_no_proxy: bool,
    pub allow_mainnet: bool,
    pub confirmations_daa: u64,
    /// The weakest finality a settlement is answered at (`accepted` or `confirmed`, default `confirmed`): the stronger of it and
    /// the offer's. `accepted` answers once the transaction is in the virtual chain (depth 0: a payer can still race a
    /// conflicting spend through another node); `confirmed` waits `confirmationsDaa` more (about 10 s at the default 100).
    pub min_finality: String,
    pub max_fee_sompi: u64,
    pub min_amount_sompi: u64,
    pub swap: bool,
    /// `builtin` (the registry shipped with this binary) or a path to a registry JSON; absent = no registry tokens.
    pub registry: Option<String>,
    pub tokens: Vec<TokenCfg>,
    pub allow_issuer_controlled: bool,
    pub merchants: Vec<MerchantCfg>,
    /// SHA-256 (hex) of an admin key that may read `/metrics` from a non-loopback address.
    pub admin_key_sha256: Option<String>,
    pub rate_limit: RateLimits,
    pub max_body_bytes: usize,
    pub body_deadline_ms: u64,
    pub header_timeout_ms: u64,
    pub settle_wait_ms: u64,
    pub poll_interval_ms: u64,
    /// Re-submissions of the same verified transaction when the node could not be reached on submit (idempotent: the
    /// node answers a transaction it already has as known), before the settlement is left `ambiguous`. 0..=10.
    pub submit_retries: u32,
    /// Re-broadcasts of the same transaction while a settle observes it and it left the mempool unaccepted (an eviction).
    /// 0..=10.
    pub rebroadcasts: u32,
    pub node_timeout_ms: u64,
    pub reorg_watch_daa: u64,
    pub reconcile_interval_seconds: u64,
    pub max_concurrent_settles: usize,
    /// Most settlements one merchant may have in flight: the global cap alone lets one merchant hold every slot for
    /// `settleWaitMs` each with transactions that conflict with each other. Clamped to `maxConcurrentSettles`.
    pub max_settles_per_merchant: usize,
    /// Most anonymous `POST /invoices/{id}/pay` calls running at once. A pool of their own: they never take `/settle` slots.
    pub max_concurrent_invoice_pays: usize,
    /// Most `/verify` calls running at once (each runs the script engine on a blocking thread).
    pub max_concurrent_verifies: usize,
    pub max_connections: usize,
    /// Open connections per socket peer (IPv6: per `ipv6PrefixBits` prefix; `trustedProxies` exempt), so one host cannot hold
    /// every one of `maxConnections`; 0 = no per-address cap.
    pub max_connections_per_ip: usize,
    /// Open connections per IPv6 site (all /64s of one `ipv6SitePrefixBits` prefix together; `trustedProxies` exempt); 0 = no
    /// site cap.
    pub max_connections_per_site: usize,
    /// IPv6 clients are counted per this many leading address bits (64: one subscriber) by the connection cap and `perIp`.
    pub ipv6_prefix_bits: u8,
    /// Leading bits of an IPv6 site (48) for `maxConnectionsPerSite` and `rateLimit.perSite`; 0 disables both site limits.
    pub ipv6_site_prefix_bits: u8,
    /// A response write that makes no progress for this long (the client stopped reading) closes the connection; 0 = off.
    pub write_timeout_ms: u64,
    /// Relative to the configuration file's directory ([`X402Config::load`]).
    pub kill_switch_file: Option<String>,
    /// The process's pause file (`kob-executor run --pause-file`); not a configuration key.
    #[serde(skip)]
    pub pause_file: Option<std::path::PathBuf>,
    /// Reverse proxies (CIDR or bare IP) whose forwarded-client header is believed. Behind a proxy on the same host
    /// EVERY request comes from 127.0.0.1: without this the per-IP limits are one shared bucket, and with it `/metrics` no longer
    /// trusts the loopback peer (the operator uses the admin key).
    pub trusted_proxies: Vec<String>,
    /// Header carrying the client address behind a trusted proxy (`x-forwarded-for`: right-most address that is not a proxy).
    pub client_ip_header: String,
    /// Serve `/metrics` to a loopback peer without the admin key. Ignored (off) when `trustedProxies` is set or when the request
    /// carries any forwarding header.
    pub metrics_loopback: bool,
    /// Intent-based swap-and-pay (absent = off).
    pub intents: IntentCfg,
    /// Invoices (absent = off).
    pub invoices: InvoiceCfg,
}

impl Default for X402Config {
    fn default() -> Self {
        X402Config {
            network: "kaspa:testnet-10".into(),
            node: "ws://127.0.0.1:18210".into(),
            listen: "127.0.0.1:8402".into(),
            ledger: "x402-ledger.jsonl".into(),
            auth: AuthMode::Required,
            open_auth_no_proxy: false,
            allow_mainnet: false,
            confirmations_daa: 100,
            min_finality: "confirmed".into(),
            max_fee_sompi: 50_000_000,
            min_amount_sompi: 0,
            swap: true,
            registry: None,
            tokens: vec![],
            allow_issuer_controlled: false,
            merchants: vec![],
            admin_key_sha256: None,
            rate_limit: RateLimits::default(),
            max_body_bytes: 1024 * 1024,
            body_deadline_ms: 10_000,
            header_timeout_ms: 10_000,
            settle_wait_ms: 30_000,
            poll_interval_ms: 200,
            submit_retries: 2,
            rebroadcasts: 2,
            node_timeout_ms: 5_000,
            reorg_watch_daa: 36_000,
            reconcile_interval_seconds: 30,
            max_concurrent_settles: 64,
            max_settles_per_merchant: 8,
            max_concurrent_invoice_pays: 16,
            max_concurrent_verifies: 64,
            max_connections: 1024,
            max_connections_per_ip: 64,
            max_connections_per_site: 128,
            ipv6_prefix_bits: 64,
            ipv6_site_prefix_bits: 48,
            write_timeout_ms: 30_000,
            kill_switch_file: None,
            pause_file: None,
            trusted_proxies: vec![],
            client_ip_header: "x-forwarded-for".into(),
            metrics_loopback: true,
            intents: IntentCfg::default(),
            invoices: InvoiceCfg::default(),
        }
    }
}

/// A validated merchant.
#[derive(Clone, Debug)]
pub struct Merchant {
    pub id: String,
    pub key_hash: [u8; 32],
    pub allowed_pay_to: HashSet<String>,
    pub allowed_assets: HashSet<String>,
    pub rate: Option<Rate>,
}

impl Merchant {
    /// The identity used when authentication is `open` (loopback only).
    pub fn open() -> Merchant {
        Merchant { id: "open".into(), key_hash: [0; 32], allowed_pay_to: HashSet::new(), allowed_assets: HashSet::new(), rate: None }
    }
}

/// A validated configuration: everything the service needs, parsed.
#[derive(Clone, Debug)]
pub struct Built {
    pub network: Network,
    pub policy: Policy,
    pub merchants: Vec<Merchant>,
    pub admin_key_hash: Option<[u8; 32]>,
    pub listen: SocketAddr,
    pub cfg: X402Config,
}

impl X402Config {
    /// Transport bounds of the HTTP server (`crate::api::conn`).
    pub fn conn_limits(&self) -> crate::api::conn::ConnLimits {
        crate::api::conn::ConnLimits {
            header_timeout: std::time::Duration::from_millis(self.header_timeout_ms.max(1)),
            write_timeout: std::time::Duration::from_millis(self.write_timeout_ms),
            max_connections: self.max_connections.max(1),
            max_per_ip: self.max_connections_per_ip,
            ipv6_prefix_bits: self.ipv6_prefix_bits,
            max_per_site: self.max_connections_per_site,
            ipv6_site_prefix_bits: self.ipv6_site_prefix_bits,
            exempt: crate::api::client_ip::TrustedProxies::parse(&self.trusted_proxies).unwrap_or_default(),
        }
    }

    /// Parses a config document (unknown fields are rejected).
    pub fn from_json(text: &str) -> Result<X402Config, ConfigError> {
        serde_json::from_str(text).map_err(|e| ConfigError(e.to_string()))
    }

    /// Reads and parses a config file.
    /// Reads the configuration file. A relative `killSwitchFile` is resolved against the file's directory (not the process's
    /// working directory, which is `/` under systemd).
    pub fn load(path: impl AsRef<Path>) -> Result<X402Config, ConfigError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError(format!("{}: {e}", path.display())))?;
        let mut cfg = Self::from_json(&text)?;
        let abs = std::path::absolute(path).map_err(|e| ConfigError(format!("{}: {e}", path.display())))?;
        let dir = abs.parent().unwrap_or(Path::new("/")).to_path_buf();
        // every file the configuration names is relative to the configuration file's directory, never to the working directory
        // (under systemd that is `/`): otherwise a start from another directory opens a fresh, empty ledger
        let rebase = |field: &str, v: &mut Option<String>| -> Result<(), ConfigError> {
            let Some(p) = v.as_deref().filter(|p| !p.trim().is_empty() && *p != ":memory:" && Path::new(p).is_relative()) else {
                return Ok(());
            };
            let next = dir.join(p);
            // a store a build before this one created next to the working directory is not left behind silently
            let old = std::path::absolute(p).map_err(|e| ConfigError(format!("{field} {p:?}: {e}")))?;
            if old != next && old.exists() && !next.exists() {
                return Err(ConfigError(format!(
                    "{field} {p:?} is relative to the configuration file's directory ({}), where it does not exist, while {} \
                     does: set {field} to the absolute path of the file in use",
                    next.display(),
                    old.display()
                )));
            }
            *v = Some(next.to_string_lossy().into_owned());
            Ok(())
        };
        let mut ledger = Some(cfg.ledger.clone());
        rebase("ledger", &mut ledger)?;
        cfg.ledger = ledger.unwrap_or_default();
        rebase("invoices.store", &mut cfg.invoices.store)?;
        // an input the facilitator only reads: a wrong place fails loudly at startup
        if let Some(r) = cfg.registry.as_deref().filter(|r| *r != "builtin" && Path::new(r).is_relative()) {
            cfg.registry = Some(dir.join(r).to_string_lossy().into_owned());
        }
        if let Some(k) = cfg.kill_switch_file.as_deref().filter(|k| Path::new(k).is_relative()) {
            cfg.kill_switch_file = Some(dir.join(k).to_string_lossy().into_owned());
        }
        Ok(cfg)
    }

    pub fn body_deadline(&self) -> Duration {
        Duration::from_millis(self.body_deadline_ms)
    }

    /// Validates everything strictly and derives the runtime structures.
    pub fn build(&self) -> Result<Built, ConfigError> {
        let network = Network::parse(&self.network)
            .ok_or_else(|| ConfigError(format!("network {:?} is not kaspa:mainnet or kaspa:testnet-10", self.network)))?;
        if network == Network::Mainnet && !self.allow_mainnet {
            return err("mainnet needs \"allowMainnet\": true (the binding's mainnet enablement requires independently corroborated chain evidence)");
        }
        if !self.node.starts_with("ws://") {
            return err("node must be a ws:// wRPC JSON url (put a TLS-terminating proxy in front of a remote node)");
        }
        let listen: SocketAddr = self.listen.parse().map_err(|e| ConfigError(format!("listen {:?}: {e}", self.listen)))?;
        if self.auth == AuthMode::Open && !listen.ip().is_loopback() {
            return err("auth \"open\" is only accepted when listening on a loopback address");
        }
        if self.auth == AuthMode::Open && !self.trusted_proxies.is_empty() {
            return err("auth \"open\" cannot be combined with trustedProxies: behind a reverse proxy every caller would use the facilitator without a key");
        }
        if self.auth == AuthMode::Open && !self.open_auth_no_proxy {
            return err(
                "auth \"open\" needs \"openAuthNoProxy\": true, the operator's statement that nothing on this host relays connections \
                 to the listen address (a reverse proxy, TLS terminator, port forwarder or tunnel connects from the loopback address \
                 and need not add a header, so it cannot be told from a local caller); with one, use auth \"required\"",
            );
        }
        if self.ledger.trim().is_empty() {
            return err("ledger path must not be empty (use \":memory:\" for a volatile ledger)");
        }
        if self.ledger == ":memory:" && network == Network::Mainnet {
            return err("a volatile ledger (\":memory:\") is refused on mainnet: replay protection and payment-identifier idempotency vanish on restart");
        }
        for (name, v) in [
            ("bodyDeadlineMs", self.body_deadline_ms),
            ("headerTimeoutMs", self.header_timeout_ms),
            ("settleWaitMs", self.settle_wait_ms),
            ("pollIntervalMs", self.poll_interval_ms),
            ("nodeTimeoutMs", self.node_timeout_ms),
            ("reconcileIntervalSeconds", self.reconcile_interval_seconds),
        ] {
            if v == 0 {
                return err(format!("{name} must be a positive finite value"));
            }
        }
        if self.submit_retries > 10 || self.rebroadcasts > 10 {
            return err("submitRetries and rebroadcasts must be within 0..=10");
        }
        if self.max_body_bytes == 0 || self.max_body_bytes > 64 * 1024 * 1024 {
            return err("maxBodyBytes must be within 1..=67108864");
        }
        if self.max_concurrent_settles == 0 || self.max_connections == 0 {
            return err("maxConcurrentSettles and maxConnections must be positive");
        }
        if self.max_settles_per_merchant == 0 || self.max_concurrent_verifies == 0 || self.max_concurrent_invoice_pays == 0 {
            return err("maxSettlesPerMerchant, maxConcurrentVerifies and maxConcurrentInvoicePays must be positive");
        }
        crate::api::client_ip::TrustedProxies::parse(&self.trusted_proxies)
            .map_err(|e| ConfigError(format!("trustedProxies: {e}")))?;
        if self.client_ip_header.trim().is_empty() {
            return err("clientIpHeader must not be empty");
        }
        if self.intents.enabled {
            let k =
                parse_hex32(&self.intents.keeper_pubkey).ok_or_else(|| ConfigError("intents.keeperPubkey must be 64 hex".into()))?;
            if secp256k1::XOnlyPublicKey::from_slice(&k).is_err() {
                return err("intents.keeperPubkey is not a valid x-only public key");
            }
            if !self.swap {
                return err("intents need \"swap\": true");
            }
            if self.intents.filler_sompi < 1_000_000
                || self.intents.max_attempts == 0
                || self.intents.max_builds == 0
                || self.intents.max_candidates == 0
            {
                return err("intents: fillerSompi >= 1000000 and positive maxAttempts / maxBuilds / maxCandidates are required");
            }
            kob_protocol::router::check_router().map_err(|e| ConfigError(format!("intents: the embedded router: {e}")))?;
        }
        if self.invoices.enabled {
            if self.invoices.max_lifetime_seconds == 0 || self.invoices.max_open_per_merchant == 0 {
                return err("invoices: maxLifetimeSeconds and maxOpenPerMerchant must be positive");
            }
            if let Some(u) = &self.invoices.public_url {
                if !(u.starts_with("https://") || u.starts_with("http://")) {
                    return err("invoices.publicUrl must be an http(s) URL");
                }
            }
            if self.invoices.store.as_deref() == Some(":memory:") && network == Network::Mainnet {
                return err("a volatile invoice store is refused on mainnet");
            }
        }
        check_rate("rateLimit.perIp", self.rate_limit.per_ip)?;
        check_rate("rateLimit.perSite", self.rate_limit.per_site)?;
        check_rate("rateLimit.anonymous", self.rate_limit.anonymous)?;
        if !(32..=128).contains(&self.ipv6_prefix_bits) {
            return err("ipv6PrefixBits must be within 32..=128");
        }
        if self.ipv6_site_prefix_bits != 0 && !(16..=self.ipv6_prefix_bits).contains(&self.ipv6_site_prefix_bits) {
            return err("ipv6SitePrefixBits must be 0 (off) or within 16..=ipv6PrefixBits");
        }
        check_rate("rateLimit.perMerchant", self.rate_limit.per_merchant)?;

        let mut policy = Policy::new(network);
        policy.confirmations_daa = self.confirmations_daa;
        policy.min_finality = match self.min_finality.as_str() {
            "accepted" => Finality::Accepted,
            "confirmed" => Finality::Confirmed,
            other => return err(format!("minFinality {other:?} is not accepted or confirmed")),
        };
        policy.limits.max_fee_sompi = self.max_fee_sompi;
        policy.limits.min_amount_sompi = self.min_amount_sompi;
        policy.limits.max_body_bytes = self.max_body_bytes;
        policy.swap_enabled = self.swap;
        policy.allow_issuer_controlled = self.allow_issuer_controlled;
        policy.tokens = self.build_tokens(network)?;

        let mut merchants: Vec<Merchant> = Vec::new();
        let mut seen = HashSet::new();
        for m in &self.merchants {
            if m.id.is_empty()
                || m.id.len() > 64
                || !m.id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
            {
                return err(format!("merchant id {:?} must match [A-Za-z0-9_.-]{{1,64}}", m.id));
            }
            if !seen.insert(m.id.clone()) {
                return err(format!("merchant id {:?} is listed twice", m.id));
            }
            let key_hash = parse_hex32(&m.api_key_sha256)
                .ok_or_else(|| ConfigError(format!("merchant {}: apiKeySha256 must be 64 lowercase hex", m.id)))?;
            if merchants.iter().any(|o| o.key_hash == key_hash) {
                return err(format!("merchant {}: apiKeySha256 is shared with another merchant", m.id));
            }
            if m.allowed_pay_to.is_empty() || m.allowed_assets.is_empty() {
                return err(format!("merchant {}: allowedPayTo and allowedAssets must not be empty (fail closed)", m.id));
            }
            for a in &m.allowed_pay_to {
                let addr =
                    Address::try_from(a.as_str()).map_err(|e| ConfigError(format!("merchant {}: allowedPayTo {a:?}: {e}", m.id)))?;
                if addr.prefix != network.prefix() {
                    return err(format!("merchant {}: allowedPayTo {a:?} is not a {network} address", m.id));
                }
            }
            for a in &m.allowed_assets {
                if a != kob_x402::wire::ASSET_KAS && parse_hex32(a).is_none() {
                    return err(format!("merchant {}: allowedAssets entry {a:?} is neither KAS nor a covenant id", m.id));
                }
            }
            if let Some(r) = m.rate_limit {
                check_rate(&format!("merchant {} rateLimit", m.id), r)?;
            }
            merchants.push(Merchant {
                id: m.id.clone(),
                key_hash,
                allowed_pay_to: m.allowed_pay_to.iter().cloned().collect(),
                allowed_assets: m.allowed_assets.iter().cloned().collect(),
                rate: m.rate_limit.map(RateCfg::rate),
            });
        }
        if self.auth == AuthMode::Required && merchants.is_empty() {
            return err("auth \"required\" needs at least one merchant");
        }
        if self.auth == AuthMode::Open && !merchants.is_empty() {
            return err("auth \"open\" and a merchant list contradict each other");
        }
        let admin_key_hash = match &self.admin_key_sha256 {
            Some(h) => Some(parse_hex32(h).ok_or_else(|| ConfigError("adminKeySha256 must be 64 lowercase hex".into()))?),
            None => None,
        };
        Ok(Built { network, policy, merchants, admin_key_hash, listen, cfg: self.clone() })
    }

    fn build_tokens(&self, network: Network) -> Result<TokenAllowlist, ConfigError> {
        let mut list = match self.registry.as_deref() {
            None => TokenAllowlist::new(),
            Some(src) => {
                let reg = if src == "builtin" {
                    Registry::default_registry()
                } else {
                    let text = std::fs::read_to_string(src).map_err(|e| ConfigError(format!("registry {src}: {e}")))?;
                    Registry::parse(&text).map_err(|e| ConfigError(format!("registry {src}: {e}")))?
                };
                reg.validate_for_network(network.registry_name()).map_err(|errs| {
                    ConfigError(format!("registry {src}: {}", errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; ")))
                })?;
                TokenAllowlist::from_registry(&reg)
            }
        };
        for t in &self.tokens {
            let cov = parse_hex32(&t.covenant_id)
                .ok_or_else(|| ConfigError(format!("token {}: covenantId must be 64 lowercase hex", t.ticker)))?;
            let hash = parse_hex32(&t.program_template_hash)
                .ok_or_else(|| ConfigError(format!("token {}: programTemplateHash must be 64 lowercase hex", t.ticker)))?;
            let tpl = token_template_by_hash(&hash)
                .ok_or_else(|| ConfigError(format!("token {}: programTemplateHash is not a pinned token program", t.ticker)))?;
            let program = tpl.id;
            let family = match t.family.as_deref() {
                None => tpl.family,
                Some("kcc20") => Family::Kcc20,
                Some("kron") => Family::Kron,
                Some(o) => return Err(ConfigError(format!("token {}: family must be kcc20 or kron, not {o:?}", t.ticker))),
            };
            if family != tpl.family {
                return Err(ConfigError(format!(
                    "token {}: programTemplateHash is a {} program, not {}",
                    t.ticker,
                    tpl.family.as_str(),
                    family.as_str()
                )));
            }
            let ext = match (family, t.extension_commitment.as_str()) {
                (Family::Kron, "") => [0; 32],
                _ => parse_hex32(&t.extension_commitment)
                    .ok_or_else(|| ConfigError(format!("token {}: extensionCommitment must be 64 lowercase hex", t.ticker)))?,
            };
            let custody = Custody::parse(&t.custody)
                .ok_or_else(|| ConfigError(format!("token {}: custody must be unconditional or issuer-controlled", t.ticker)))?;
            list.insert(AllowedToken {
                covenant_id: cov,
                program,
                family,
                extension_commitment: ext,
                custody,
                ticker: t.ticker.clone(),
                decimals: t.decimals,
            })
            .map_err(|e| ConfigError(format!("token {}: {e}", t.ticker)))?;
        }
        Ok(list)
    }
}

fn check_rate(what: &str, r: RateCfg) -> Result<(), ConfigError> {
    if r.burst == 0 || !(r.per_second.is_finite() && r.per_second > 0.0) {
        return err(format!("{what}: burst and perSecond must be positive"));
    }
    Ok(())
}

/// SHA-256 of an API key (what `apiKeySha256` holds).
pub fn api_key_hash(key: &str) -> [u8; 32] {
    Sha256::digest(key.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merchant_json(key: &str) -> String {
        let addr = kob_x402::testkit::pubkey(1);
        let a = Address::new(kaspa_addresses::Prefix::Testnet, kaspa_addresses::Version::PubKey, &addr).to_string();
        format!(
            r#"{{"id":"shop-1","apiKeySha256":"{}","allowedPayTo":["{a}"],"allowedAssets":["KAS"]}}"#,
            kob_x402::wire::hex(&api_key_hash(key))
        )
    }

    fn doc(extra: &str) -> String {
        format!(r#"{{"merchants":[{}]{extra}}}"#, merchant_json("secret"))
    }

    #[test]
    fn minimal_config_builds_with_defaults() {
        let b = X402Config::from_json(&doc("")).unwrap().build().unwrap();
        assert_eq!(b.network, Network::Testnet10);
        assert_eq!(b.policy.confirmations_daa, 100);
        assert_eq!(b.merchants.len(), 1);
        assert_eq!(b.cfg.body_deadline(), Duration::from_secs(10));
        assert!(b.policy.tokens.iter().next().is_none());
        assert!(b.policy.swap_enabled);
        // a settlement is answered at `confirmed` unless the operator lowers it
        assert_eq!(b.policy.min_finality, Finality::Confirmed);
        let a = X402Config::from_json(&doc(r#","minFinality":"accepted""#)).unwrap().build().unwrap();
        assert_eq!(a.policy.min_finality, Finality::Accepted);
        assert!(X402Config::from_json(&doc(r#","minFinality":"mempool""#)).unwrap().build().is_err());
    }

    #[test]
    fn unknown_fields_are_rejected_everywhere() {
        assert!(X402Config::from_json(&doc(r#","bogus":1"#)).is_err());
        assert!(X402Config::from_json(r#"{"rateLimit":{"perIp":{"burst":1,"perSecond":1,"x":2}}}"#).is_err());
        let m = merchant_json("k").replace(r#""allowedAssets""#, r#""extra":1,"allowedAssets""#);
        assert!(X402Config::from_json(&format!(r#"{{"merchants":[{m}]}}"#)).is_err());
        assert!(X402Config::from_json(
            r#"{"tokens":[{"covenantId":"","programTemplateHash":"","extensionCommitment":"","custody":"x","zzz":1}]}"#
        )
        .is_err());
    }

    #[test]
    fn auth_open_requires_loopback_and_no_merchants() {
        let ok = X402Config::from_json(r#"{"auth":"open","openAuthNoProxy":true,"listen":"127.0.0.1:8402"}"#).unwrap().build();
        assert!(ok.is_ok());
        // a same-host relay without forwarding headers cannot be told from a local caller: the operator must state there is none
        let e = X402Config::from_json(r#"{"auth":"open","listen":"127.0.0.1:8402"}"#).unwrap().build().unwrap_err();
        assert!(e.0.contains("openAuthNoProxy"), "{}", e.0);
        let e =
            X402Config::from_json(r#"{"auth":"open","openAuthNoProxy":true,"listen":"0.0.0.0:8402"}"#).unwrap().build().unwrap_err();
        assert!(e.0.contains("loopback"));
        let e = X402Config::from_json(r#"{"auth":"open","openAuthNoProxy":true,"listen":"[::]:8402"}"#).unwrap().build().unwrap_err();
        assert!(e.0.contains("loopback"));
        assert!(X402Config::from_json(r#"{"auth":"open","openAuthNoProxy":true,"listen":"[::1]:8402"}"#).unwrap().build().is_ok());
        let e = X402Config::from_json(&doc(r#","auth":"open","openAuthNoProxy":true"#)).unwrap().build().unwrap_err();
        assert!(e.0.contains("contradict"));
        // behind a reverse proxy (declared as trusted) open auth would serve every caller
        let e = X402Config::from_json(
            r#"{"auth":"open","openAuthNoProxy":true,"listen":"127.0.0.1:8402","trustedProxies":["127.0.0.1"]}"#,
        )
        .unwrap()
        .build()
        .unwrap_err();
        assert!(e.0.contains("trustedProxies"), "{}", e.0);
    }

    #[test]
    fn the_shipped_example_config_is_valid() {
        let cfg = X402Config::from_json(include_str!("../../x402.example.json")).unwrap();
        // the builtin registry targets one network: the example must build against it
        let b = cfg.build();
        let reg_net = Registry::default_registry().network;
        if reg_net == "testnet-10" {
            let b = b.unwrap();
            assert_eq!(b.merchants.len(), 1);
            assert_eq!(b.cfg.kill_switch_file.as_deref(), Some("x402.kill"));
        } else {
            assert!(b.is_err());
        }
    }

    #[test]
    fn a_relative_kill_switch_file_is_next_to_the_configuration_file() {
        let d = tempfile::tempdir().unwrap();
        let cfg_dir = d.path().join("etc");
        std::fs::create_dir(&cfg_dir).unwrap();
        let path = cfg_dir.join("x402.json");
        let base =
            r#"{"network":"kaspa:testnet-10","auth":"open","openAuthNoProxy":true,"listen":"127.0.0.1:8402","ledger":":memory:""#;
        std::fs::write(&path, format!(r#"{base},"killSwitchFile":"x402.kill"}}"#)).unwrap();
        let c = X402Config::load(&path).unwrap();
        assert_eq!(c.kill_switch_file.as_deref().map(std::path::PathBuf::from), Some(cfg_dir.join("x402.kill")));
        // absolute on every platform (`/var/...` has no drive on Windows, so it would be relative there)
        let abs = d.path().join("lib").join("x402.kill");
        let abs = abs.to_str().unwrap();
        std::fs::write(&path, format!(r#"{base},"killSwitchFile":{}}}"#, serde_json::to_string(abs).unwrap())).unwrap();
        assert_eq!(X402Config::load(&path).unwrap().kill_switch_file.as_deref(), Some(abs));
        std::fs::write(&path, format!("{base}}}")).unwrap();
        assert_eq!(X402Config::load(&path).unwrap().kill_switch_file, None);
    }

    /// The ledger and the invoice store are next to the configuration file too, whatever the working directory (under systemd:
    /// `/`); a store a build before found next to the working directory is not left behind silently.
    #[test]
    fn relative_ledger_and_store_are_next_to_the_configuration_file() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("x402.json");
        let base = r#"{"auth":"open","openAuthNoProxy":true,"listen":"127.0.0.1:8402""#;
        std::fs::write(&path, format!(r#"{base},"ledger":"state/ledger.jsonl","invoices":{{"store":"inv.jsonl"}}}}"#)).unwrap();
        let c = X402Config::load(&path).unwrap();
        assert_eq!(std::path::PathBuf::from(&c.ledger), d.path().join("state/ledger.jsonl"));
        assert_eq!(c.invoices.store.as_deref().map(std::path::PathBuf::from), Some(d.path().join("inv.jsonl")));
        // absolute and volatile paths are kept; the default ledger name is next to the file as well
        std::fs::write(&path, format!(r#"{base},"ledger":":memory:","invoices":{{"store":":memory:"}}}}"#)).unwrap();
        let c = X402Config::load(&path).unwrap();
        assert_eq!((c.ledger.as_str(), c.invoices.store.as_deref()), (":memory:", Some(":memory:")));
        std::fs::write(&path, format!("{base}}}")).unwrap();
        assert_eq!(std::path::PathBuf::from(X402Config::load(&path).unwrap().ledger), d.path().join("x402-ledger.jsonl"));
        // an existing ledger relative to the working directory, none next to the configuration file: refused
        let cwd = std::env::current_dir().unwrap();
        let here = tempfile::tempdir_in(&cwd).unwrap();
        let name = here.path().file_name().unwrap().to_string_lossy().into_owned();
        std::fs::write(here.path().join("old.jsonl"), b"").unwrap();
        std::fs::write(&path, format!(r#"{base},"ledger":"{name}/old.jsonl"}}"#)).unwrap();
        let e = X402Config::load(&path).unwrap_err();
        assert!(e.0.contains("absolute path"), "{}", e.0);
    }

    #[test]
    fn intents_and_invoices_are_validated() {
        let key = kob_x402::wire::hex(&kob_x402::testkit::pubkey(3));
        let ok = X402Config::from_json(&doc(&format!(
            r#","intents":{{"enabled":true,"keeperPubkey":"{key}"}},"invoices":{{"enabled":true}}"#
        )));
        assert!(ok.unwrap().build().is_ok());
        for (bad, why) in [
            (r#","intents":{"enabled":true,"keeperPubkey":"zz"}"#.to_string(), "keeperPubkey"),
            (format!(r#","intents":{{"enabled":true,"keeperPubkey":"{}"}}"#, "00".repeat(32)), "x-only"),
            (format!(r#","swap":false,"intents":{{"enabled":true,"keeperPubkey":"{key}"}}"#), "swap"),
            (r#","invoices":{"enabled":true,"publicUrl":"ftp://x"}"#.to_string(), "publicUrl"),
            (r#","invoices":{"enabled":true,"maxLifetimeSeconds":0}"#.to_string(), "maxLifetimeSeconds"),
            (r#","intents":{"enabled":true,"unknown":1}"#.to_string(), "unknown"),
        ] {
            let e = X402Config::from_json(&doc(&bad)).and_then(|c| c.build().map(|_| ())).unwrap_err();
            assert!(e.0.contains(why), "{bad}: {e}");
        }
    }

    #[test]
    fn required_auth_needs_a_merchant() {
        assert!(X402Config::from_json("{}").unwrap().build().is_err());
    }

    #[test]
    fn mainnet_needs_an_explicit_flag() {
        let e = X402Config::from_json(r#"{"auth":"open","network":"kaspa:mainnet"}"#).unwrap().build().unwrap_err();
        assert!(e.0.contains("allowMainnet"));
        assert!(X402Config::from_json(r#"{"auth":"open","openAuthNoProxy":true,"network":"kaspa:mainnet","allowMainnet":true}"#)
            .unwrap()
            .build()
            .is_ok());
    }

    #[test]
    fn strict_value_validation() {
        for bad in [
            r#","network":"mainnet""#,
            r#","node":"wss://x""#,
            r#","listen":"nope""#,
            r#","bodyDeadlineMs":0"#,
            r#","maxBodyBytes":0"#,
            r#","rateLimit":{"perIp":{"burst":0,"perSecond":1}}"#,
            r#","adminKeySha256":"zz""#,
            r#","ledger":"""#,
            r#","submitRetries":11"#,
            r#","rebroadcasts":11"#,
        ] {
            let cfg = X402Config::from_json(&doc(bad)).unwrap();
            assert!(cfg.build().is_err(), "{bad}");
        }
    }

    #[test]
    fn merchant_validation() {
        let good = merchant_json("k");
        let cases = [
            good.replace("shop-1", "bad id"),
            good.replace(r#""allowedAssets":["KAS"]"#, r#""allowedAssets":[]"#),
            good.replace(r#""allowedAssets":["KAS"]"#, r#""allowedAssets":["XYZ"]"#),
            good.replacen("kaspatest:", "kaspa:", 1),
            good.replace(r#""allowedPayTo":["#, r#""allowedPayTo":["nonsense","#),
        ];
        for m in cases {
            let cfg = X402Config::from_json(&format!(r#"{{"merchants":[{m}]}}"#)).unwrap();
            assert!(cfg.build().is_err(), "{m}");
        }
        // duplicate ids / shared keys
        let cfg = X402Config::from_json(&format!(r#"{{"merchants":[{good},{good}]}}"#)).unwrap();
        assert!(cfg.build().is_err());
        let other = good.replace("shop-1", "shop-2");
        let cfg = X402Config::from_json(&format!(r#"{{"merchants":[{good},{other}]}}"#)).unwrap();
        assert!(cfg.build().unwrap_err().0.contains("shared"));
    }

    #[test]
    fn tokens_from_the_builtin_registry_and_operator_entries() {
        let reg = Registry::default_registry();
        let net = reg.network.clone();
        let cfg = X402Config::from_json(&doc(r#","registry":"builtin""#)).unwrap();
        let built = cfg.build();
        // the builtin registry targets a specific network; the config network must match it
        if net == "testnet-10" {
            assert!(built.is_ok(), "{:?}", built.err());
        } else {
            assert!(built.is_err());
        }
        // an operator token needs a pinned token program
        let cfg = X402Config::from_json(&doc(&format!(
            r#","tokens":[{{"covenantId":"{}","programTemplateHash":"{}","extensionCommitment":"{}","custody":"issuer-controlled","ticker":"T","decimals":2}}]"#,
            "aa".repeat(32),
            "bb".repeat(32),
            "cc".repeat(32)
        )))
        .unwrap();
        assert!(cfg.build().unwrap_err().0.contains("pinned token program"));
        let prog = kob_protocol::artifacts::template(kob_protocol::artifacts::TemplateId::Kcc20Ref);
        let cfg = X402Config::from_json(&doc(&format!(
            r#","tokens":[{{"covenantId":"{}","programTemplateHash":"{}","extensionCommitment":"{}","custody":"issuer-controlled","ticker":"T","decimals":2}}]"#,
            "aa".repeat(32),
            kob_x402::wire::hex(&prog.hash),
            "cc".repeat(32)
        )))
        .unwrap();
        let b = cfg.build().unwrap();
        let t = b.policy.tokens.find(&[0xaa; 32]).unwrap();
        assert_eq!(t.custody, Custody::IssuerControlled);
        // a template that is not a token program (an order template) is refused
        let ask = kob_protocol::artifacts::template(kob_protocol::artifacts::TemplateId::KobAsk);
        let cfg = X402Config::from_json(&doc(&format!(
            r#","tokens":[{{"covenantId":"{}","programTemplateHash":"{}","extensionCommitment":"{}","custody":"unconditional"}}]"#,
            "aa".repeat(32),
            kob_x402::wire::hex(&ask.hash),
            "cc".repeat(32)
        )))
        .unwrap();
        assert!(cfg.build().is_err());
    }

    #[test]
    fn operator_tokens_of_either_family_and_the_kaspacom_template() {
        use kob_protocol::artifacts::{token_template, TemplateId};
        let hex = kob_x402::wire::hex;
        let tok = |cov: &str, prog: TemplateId, extra: &str| {
            format!(
                r#","tokens":[{{"covenantId":"{}","programTemplateHash":"{}","custody":"unconditional","ticker":"T"{extra}}}]"#,
                cov.repeat(32),
                hex(&token_template(prog).hash)
            )
        };
        // a KRON token: the family is the program's, no extension commitment, a pay asset only
        let b = X402Config::from_json(&doc(&tok("aa", TemplateId::KronToken2433, ""))).unwrap().build().unwrap();
        let t = b.policy.tokens.find(&[0xaa; 32]).unwrap();
        assert_eq!((t.family, t.extension_commitment, t.is_merchant_capable()), (Family::Kron, [0; 32], false));
        // ... which may say its family, but not a wrong one
        let ok = tok("aa", TemplateId::KronToken2732, r#","family":"kron""#);
        assert!(X402Config::from_json(&doc(&ok)).unwrap().build().is_ok());
        let wrong = tok("aa", TemplateId::KronToken2732, r#","family":"kcc20""#);
        assert!(X402Config::from_json(&doc(&wrong)).unwrap().build().unwrap_err().0.contains("kron program"));
        let junk = tok("aa", TemplateId::KronToken2732, r#","family":"erc20""#);
        assert!(X402Config::from_json(&doc(&junk)).unwrap().build().is_err());
        // a KRON token has no extension commitment
        let with_ext = tok("aa", TemplateId::KronToken2433, &format!(r#","extensionCommitment":"{}""#, "cc".repeat(32)));
        assert!(X402Config::from_json(&doc(&with_ext)).unwrap().build().is_err());
        // a KCC-20 token needs one
        assert!(X402Config::from_json(&doc(&tok("aa", TemplateId::Kcc20Ref8x8, ""))).unwrap().build().is_err());
        // KaspaCom's template is an ordinary KCC-20 program of the strict list
        let kc = tok("bb", TemplateId::Kcc20KaspaCom025, &format!(r#","extensionCommitment":"{}""#, "cc".repeat(32)));
        let b = X402Config::from_json(&doc(&kc)).unwrap().build().unwrap();
        let t = b.policy.tokens.find(&[0xbb; 32]).unwrap();
        assert_eq!((t.program, t.family, t.is_merchant_capable()), (TemplateId::Kcc20KaspaCom025, Family::Kcc20, true));
    }
}
