//! Command-line arguments and entry points of `kobd match` and `kobd keep`.
//!
//! The secret key is never an argument: `--key-file PATH`, `$KOB_OPERATOR_KEY_FILE`, a systemd
//! credential or `$KOB_OPERATOR_KEY` (see [`super::wallet`]).

use std::path::PathBuf;
use std::time::Duration;

use clap::Args;

use super::book::MemoryBook;
use super::engine::{tick, EngineConfig, TickInput};
use super::family::Families;
use super::node::{WrpcConfig, WrpcNode};
use super::run::{read_snapshot, FileSource, Roles, RunConfig, Runner};
use super::wallet::{HotKey, Signer};
use crate::keepers::KeeperConfig;
use crate::shutdown::shutdown_signal;

/// Options shared by `match` and `keep`.
#[derive(Args, Debug, Clone)]
pub struct CommonArgs {
    /// Network id the node must report (`mainnet`, `testnet-10`). Required on the reference build
    /// (no default); a deployment build (`deploy-mainnet`, `deploy-tn10`) implies its own and
    /// refuses another. Not needed with `--offline`.
    #[arg(long, env = "KOB_NETWORK")]
    pub network: Option<String>,
    /// Node JSON wRPC endpoint (keep it on loopback). Default: the local node's port of the network
    /// (`ws://127.0.0.1:18110` on mainnet, `ws://127.0.0.1:18210` on testnet-10). Command-line
    /// arguments are visible to other local users (`ps`): a URL with a password or an API key
    /// belongs in `KOB_NODE_WRPC`; logs show it redacted.
    #[arg(long, env = "KOB_NODE_WRPC")]
    pub rpc_url: Option<String>,
    /// Book snapshot (JSON, `MemoryBook`), re-read when it changes. Written by
    /// `kobd db export-book --out FILE`.
    #[arg(long, env = "KOB_BOOK_FILE")]
    pub book_file: PathBuf,
    /// File holding the operator's secret key (64 hex, mode 0600). Not the key itself.
    #[arg(long, env = "KOB_OPERATOR_KEY_FILE")]
    pub key_file: Option<PathBuf>,
    /// Folder of what must survive a restart: the in-flight journal, the keepers' subsidy window
    /// and the start count, as `kobd --executor` keeps them in its data directory. One folder per
    /// process; it is created if missing. Without it they live in memory only, so the keepers pay
    /// no subsidy: a restart would start a fresh budget (R4F-2).
    #[arg(long, env = "KOB_STATE_DIR")]
    pub state_dir: Option<PathBuf>,
    /// Build and validate everything, submit nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Plan once against the snapshot without a node (implies --dry-run; ephemeral key and
    /// synthetic funding when no key is configured). Prints the plans as JSON.
    #[arg(long)]
    pub offline: bool,
    /// Tick period in milliseconds.
    #[arg(long, default_value_t = 1_000)]
    pub tick_ms: u64,
    /// The lowest fee rate, sompi per gram (>= 100); with the node's fee estimate each transaction
    /// pays its urgency's bucket within `--fee-max-rate` and `--fee-max-tx-kas`
    /// (`docs/ops/executor.md`, fee policy).
    #[arg(long, default_value_t = 100)]
    pub fee_rate: u64,
    #[command(flatten)]
    pub fees: crate::fee::FeeArgs,
    /// Kill switch: nothing is built or submitted while this file exists. A file whose state cannot
    /// be read (permission denied) or whose directory does not exist counts as present. Keep it on
    /// persistent storage.
    #[arg(long)]
    pub pause_file: Option<PathBuf>,
    /// Prometheus textfile-collector output (`.prom`).
    #[arg(long)]
    pub metrics_file: Option<PathBuf>,
    /// Warn (and set `kob_operator_low_funds`) below this many KAS of funding.
    #[arg(long, default_value_t = 100)]
    pub low_funds_kas: u64,
    /// Plan nothing while the snapshot is more than this many DAA behind the node
    /// (`kob_matcher_book_stale`; a stale or no longer refreshed file). 0: never wait. Default 300
    /// (30 s), as `run`.
    #[arg(long, default_value_t = 300)]
    pub max_book_lag_daa: u64,
    #[command(flatten)]
    pub select: super::select::SelectArgs,
}

/// `kobd match`.
#[derive(Args, Debug, Clone)]
pub struct MatchArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Smallest profit per batch after its fee, sompi.
    #[arg(long, default_value_t = 1)]
    pub min_profit: i64,
    /// Optional cap on the size of one transaction, bytes. 0 (the default): the physical limit
    /// (250 000 bytes of transient mass and the block mass limits of the built transaction); a
    /// larger batch only costs its own fee.
    #[arg(long, default_value_t = 0)]
    pub max_tx_bytes: u64,
    /// Do not chain onto unaccepted parents (a partially filled order continues next tick).
    #[arg(long)]
    pub no_chain: bool,
    /// Do not arm or ratchet stops with updates in the batches (docs/spec/matcher.md section 4.3;
    /// the stops the batch's plain fills trigger are still filled next to their evidence).
    #[arg(long)]
    pub no_arm: bool,
    /// Wall-clock budget of one matcher tick in milliseconds (0: unlimited); the books left wait
    /// for the next tick.
    #[arg(long, default_value_t = 20_000)]
    pub tick_budget_ms: u64,
    /// Optional cap: best-priority candidates per (book, class, side) the planner looks at. 0 (the
    /// default): every candidate (the walks are sorted and the tick budget bounds a hostile book).
    #[arg(long, default_value_t = 0)]
    pub max_candidates_per_group: usize,
    /// The surplus-inventory policy (strict JSON, `docs/ops/executor.md`, "Surplus inventory"): the
    /// pair surpluses the operator may accumulate as inventory (never sold by maintenance).
    /// Default: none (off).
    #[arg(long)]
    pub inventory_policy: Option<PathBuf>,
    #[command(flatten)]
    pub liveness: crate::keepers::LivenessArgs,
    /// Also run the keepers in this process.
    #[arg(long)]
    pub keep: bool,
}

/// `kobd keep`.
#[derive(Args, Debug, Clone)]
pub struct KeepArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Do not refund / kill / close.
    #[arg(long)]
    pub no_refund: bool,
    /// Sweep strays of the operator's own orders in place (the order continues unchanged).
    #[arg(long)]
    pub sweep_own_strays: bool,
    /// Keepers: return proven foreign strays (other tokens owned by an order id) to the maker
    /// inside the refund, kill or close that ends the order (permissionless; dropped from a job
    /// that would not pay with them).
    #[arg(long)]
    pub return_foreign_strays: bool,
    #[command(flatten)]
    pub liveness: crate::keepers::LivenessArgs,
    /// Smallest profit (tip - fee) per job, sompi.
    #[arg(long, default_value_t = 0)]
    pub min_profit: i64,
}

fn engine_cfg(a: &MatchArgs) -> EngineConfig {
    let mut e = EngineConfig::default();
    e.planner.fee_rate = a.common.fee_rate;
    e.planner.min_profit = a.min_profit;
    e.planner.max_tx_bytes = a.max_tx_bytes;
    e.planner.arm = !a.no_arm;
    e.planner.max_candidates_per_group = a.max_candidates_per_group;
    e.tick_budget_ms = a.tick_budget_ms;
    e.chain_unconfirmed = !a.no_chain;
    e
}

impl CommonArgs {
    /// The network and the node endpoint of a live run ([`crate::config::resolve_network`],
    /// [`crate::config::default_wrpc_url`]). It is never an implicit testnet-10, and never a
    /// testnet node on a mainnet build.
    pub fn endpoint(&self) -> anyhow::Result<(String, String)> {
        let network = crate::config::resolve_network(self.network.as_deref()).map_err(anyhow::Error::msg)?;
        let url = match self.rpc_url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
            Some(u) => u.to_string(),
            None => crate::config::default_wrpc_url(&network)
                .ok_or_else(|| anyhow::anyhow!("no node endpoint for network {network}: pass --rpc-url"))?,
        };
        Ok((network, url))
    }
}

fn run_cfg(c: &CommonArgs, network: String, roles: Roles) -> RunConfig {
    RunConfig {
        network,
        roles,
        dry_run: c.dry_run,
        tick: Duration::from_millis(c.tick_ms.max(100)),
        pause_file: c.pause_file.clone(),
        metrics_file: c.metrics_file.clone(),
        low_funds: c.low_funds_kas * 100_000_000,
        fee_policy: c.fees.policy(c.fee_rate).unwrap_or_else(|_| crate::fee::FeePolicy::fixed(c.fee_rate)),
        max_book_lag: (c.max_book_lag_daa > 0).then_some(c.max_book_lag_daa),
        // Checked by the command before it gets here.
        select: c.select.config().unwrap_or_default(),
        state_dir: c.state_dir.clone(),
        ..RunConfig::default()
    }
}

/// The start check's paths of a keyed role ([`crate::executor::start_paths`]), with the state
/// folder (or, before it exists, the folder it is made in): its journal and budget files steer
/// what the runner resends and spends.
fn keyed_paths(c: &CommonArgs, policy: Option<&std::path::Path>) -> Vec<PathBuf> {
    let mut paths = crate::executor::start_paths(c.key_file.as_deref(), policy);
    if let Some(d) = &c.state_dir {
        let d = std::path::absolute(d).unwrap_or_else(|_| d.clone());
        paths.extend(d.ancestors().find(|a| a.exists()).map(std::path::Path::to_path_buf));
    }
    paths
}

/// A live run without a state folder keeps its journal and subsidy window in memory only. Each
/// start would bring a fresh subsidy budget, so it pays none (R4F-2). With one, the folder is
/// created.
fn state_rule(cfg: &mut RunConfig) -> anyhow::Result<()> {
    if cfg.dry_run {
        return Ok(());
    }
    match &cfg.state_dir {
        Some(d) => std::fs::create_dir_all(d).map_err(|e| anyhow::anyhow!("--state-dir {}: {e}", d.display()))?,
        None => {
            if cfg.roles.keeper && cfg.keeper.subsidy_budget > 0 && cfg.keeper.subsidy_max > 0 {
                tracing::warn!("no --state-dir: the keepers pay no subsidy (its budget would restart with each process); give --state-dir to keep it");
            }
            cfg.keeper.subsidy_budget = 0;
            tracing::warn!("no --state-dir: the in-flight journal is in memory only, so a restart forgets what this run left in the mempool and may plan its coins again (the node refuses those)");
        }
    }
    Ok(())
}

fn load_key(c: &CommonArgs) -> anyhow::Result<Box<dyn Signer>> {
    match HotKey::load(c.key_file.as_deref()) {
        Ok(k) => Ok(Box::new(k)),
        Err(e) if c.offline => {
            tracing::warn!(error = %e, "offline dry run with an ephemeral key");
            let secret: [u8; 32] = secp256k1::rand::random();
            Ok(Box::new(HotKey::from_secret(secret)?))
        }
        Err(e) => Err(e.into()),
    }
}

/// Offline planning: one tick against the snapshot, synthetic funding, JSON to stdout.
fn offline(book: &MemoryBook, engine: &EngineConfig, signer: &dyn Signer) -> anyhow::Result<()> {
    let funding = kob_protocol::tx::KeyUtxo {
        utxo: kob_protocol::tx::Utxo {
            transaction_id: [0xf0; 32],
            index: 0,
            amount: 1_000 * 100_000_000,
            block_daa_score: book.daa_score.saturating_sub(1_000),
            covenant_id: None,
        },
        pubkey: signer.pubkey(),
    };
    let inp = TickInput {
        orders: book.orders.clone(),
        clock: super::book::Clock { daa: book.daa_score, utc: super::book::utc_now() },
        funding: vec![funding],
        excluded: Default::default(),
    };
    let r = tick(&inp, engine, &Families::default(), signer);
    for p in &r.prepared {
        let fills: Vec<_> = p
            .plan
            .fills
            .iter()
            .map(|f| serde_json::json!({"order": kob_protocol::json::to_hex(&f.cand.id), "kind": f.cand.kind.name(), "leg": f.cand.leg, "amount": f.amount, "class": f.cand.class as u8, "merge": f.cand.merge.map(|m| kob_protocol::json::to_hex(&m)), "evidence": f.evidence}))
            .collect();
        let updates: Vec<_> = p
            .plan
            .updates
            .iter()
            .map(|u| serde_json::json!({"order": kob_protocol::json::to_hex(&u.id), "update": u.kind.name(), "steps": u.steps, "evidence": u.evidence, "take": u.take}))
            .collect();
        let line = serde_json::json!({
            "txid": kob_protocol::json::to_hex(&p.txid()),
            "step": p.step,
            "kind": "match",
            "lockTime": p.plan.lock_time,
            "fills": fills,
            "updates": updates,
            "fee": p.accounting.fee,
            "profit": p.accounting.profit,
            "bytes": p.accounting.bytes,
            "engineValid": p.validation.is_some(),
        });
        println!("{line}");
    }
    for (k, why) in &r.skipped {
        eprintln!("skipped {}: {why}", kob_protocol::json::to_hex(&k.token));
    }
    Ok(())
}

/// Runs `kobd match`: the matcher, and the keepers too with `--keep`.
pub async fn run_match(a: MatchArgs) -> anyhow::Result<()> {
    a.common.fees.policy(a.common.fee_rate).map_err(anyhow::Error::msg)?;
    a.common.select.config().map_err(anyhow::Error::msg)?;
    // The network and the node first (`--offline` needs neither), so nothing secret is read for a
    // run that cannot start.
    let (network, rpc_url) = if a.common.offline { Default::default() } else { a.common.endpoint()? };
    // The start check of `kobd --executor` (R3D-4): this role holds the hot key too.
    if !a.common.offline {
        crate::executor::refuse_writable_by_others(
            &network,
            &keyed_paths(&a.common, a.inventory_policy.as_deref()),
        )?;
    }
    let signer = load_key(&a.common)?;
    let mut engine = engine_cfg(&a);
    let inventory = match &a.inventory_policy {
        Some(p) => super::planner::InventoryPolicy::from_file(p).map_err(anyhow::Error::msg)?,
        None => super::planner::InventoryPolicy::default(),
    };
    engine.planner.inventory = inventory.clone();
    if a.common.offline {
        return offline(&read_snapshot(&a.common.book_file)?, &engine, signer.as_ref());
    }
    let mut cfg = run_cfg(&a.common, network, Roles { matcher: true, keeper: a.keep });
    cfg.engine = engine;
    cfg.keeper.fee_rate = a.common.fee_rate;
    a.liveness.apply(&mut cfg.keeper).map_err(anyhow::Error::msg)?;
    cfg.maintenance.fee_rate = a.common.fee_rate;
    cfg.maintenance.inventory = inventory;
    state_rule(&mut cfg)?;
    let node = WrpcNode::new(WrpcConfig::new(rpc_url.clone()));
    tracing::info!(network = %cfg.network, rpc = %crate::rpc::redact_url(&rpc_url), operator = %kob_protocol::json::to_hex(&signer.pubkey()), dry_run = cfg.dry_run, "matcher starting");
    let mut r = Runner::new(node, FileSource::new(a.common.book_file.clone()), signer, cfg);
    r.run(shutdown_signal()).await
}

/// Runs `kobd keep`: the keepers alone.
pub async fn run_keep(a: KeepArgs) -> anyhow::Result<()> {
    a.common.fees.policy(a.common.fee_rate).map_err(anyhow::Error::msg)?;
    a.common.select.config().map_err(anyhow::Error::msg)?;
    // `--offline` needs no node (and no network).
    let (network, rpc_url) = if a.common.offline { Default::default() } else { a.common.endpoint()? };
    if !a.common.offline {
        crate::executor::refuse_writable_by_others(&network, &keyed_paths(&a.common, None))?;
    }
    let signer = load_key(&a.common)?;
    let mut cfg = run_cfg(&a.common, network, Roles { matcher: false, keeper: true });
    cfg.keeper = KeeperConfig {
        fee_rate: a.common.fee_rate,
        refund: !a.no_refund,
        sweep_own_strays: a.sweep_own_strays,
        return_foreign_strays: a.return_foreign_strays,
        min_profit: a.min_profit,
        ..KeeperConfig::default()
    };
    a.liveness.apply(&mut cfg.keeper).map_err(anyhow::Error::msg)?;
    if a.common.offline {
        let book = read_snapshot(&a.common.book_file)?;
        let inp = crate::keepers::KeeperInput {
            // The listed orders and the proven unlisted ones the snapshot carries for the keepers.
            orders: book.orders.into_iter().chain(book.keeper_only).collect(),
            clock: super::book::Clock { daa: book.daa_score, utc: super::book::utc_now() },
            funding: vec![],
            excluded: Default::default(),
            excluded_outpoints: Default::default(),
            known_unprofitable: Default::default(),
        };
        let r = crate::keepers::tick(&inp, &cfg.keeper, signer.as_ref());
        for j in &r.jobs {
            println!(
                "{}",
                serde_json::json!({"kind": j.kind.name(), "order": kob_protocol::json::to_hex(&j.order), "txid": kob_protocol::json::to_hex(&j.signed.tx.id), "profit": j.profit, "engineValid": j.validation.is_some()})
            );
        }
        for (id, why) in &r.skipped {
            eprintln!("skipped {}: {why}", kob_protocol::json::to_hex(id));
        }
        return Ok(());
    }
    state_rule(&mut cfg)?;
    let node = WrpcNode::new(WrpcConfig::new(rpc_url.clone()));
    tracing::info!(network = %cfg.network, rpc = %crate::rpc::redact_url(&rpc_url), operator = %kob_protocol::json::to_hex(&signer.pubkey()), dry_run = cfg.dry_run, "keeper starting");
    let mut r = Runner::new(node, FileSource::new(a.common.book_file.clone()), signer, cfg);
    r.run(shutdown_signal()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Keep {
        #[command(flatten)]
        k: KeepArgs,
    }

    /// R4F-2: `--state-dir` gives `kobd keep` / `kobd match` the runner's state folder (journal, subsidy window,
    /// start count), created at the start; without one a live run pays no keeper subsidy, since each start would bring
    /// a fresh budget. A dry run is left alone.
    #[test]
    fn the_state_folder_keeps_the_subsidy_budget_and_without_one_there_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("keep-state");
        let a = Keep::try_parse_from(["kobd", "--book-file", "book.json", "--state-dir", state.to_str().unwrap()]).unwrap().k;
        let mut cfg = run_cfg(&a.common, "testnet-10".into(), Roles { matcher: false, keeper: true });
        a.liveness.apply(&mut cfg.keeper).unwrap();
        assert_eq!(cfg.state_dir.as_deref(), Some(state.as_path()));
        let budget = cfg.keeper.subsidy_budget;
        assert!(budget > 0);
        state_rule(&mut cfg).unwrap();
        assert!(state.is_dir(), "created at the start");
        assert_eq!(cfg.keeper.subsidy_budget, budget);
        assert!(keyed_paths(&a.common, None).contains(&state), "the start check covers it");
        // none: no subsidy on a live run, a dry run unchanged
        let a = Keep::try_parse_from(["kobd", "--book-file", "book.json"]).unwrap().k;
        let mut cfg = run_cfg(&a.common, "testnet-10".into(), Roles { matcher: false, keeper: true });
        a.liveness.apply(&mut cfg.keeper).unwrap();
        assert!(cfg.state_dir.is_none());
        let mut dry = cfg.clone();
        dry.dry_run = true;
        state_rule(&mut dry).unwrap();
        assert_eq!(dry.keeper.subsidy_budget, budget);
        state_rule(&mut cfg).unwrap();
        assert_eq!(cfg.keeper.subsidy_budget, 0);
    }
}
