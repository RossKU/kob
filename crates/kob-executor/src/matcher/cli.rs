//! Command-line arguments and entry points of `kob-executor match` and `kob-executor keep`.
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

/// Options shared by `match` and `keep`.
#[derive(Args, Debug, Clone)]
pub struct CommonArgs {
    /// Network id the node must report (`testnet-10`, `mainnet`).
    #[arg(long, default_value = "testnet-10", env = "KOB_NETWORK")]
    pub network: String,
    /// Node JSON wRPC endpoint (keep it on loopback), e.g. `ws://127.0.0.1:18210`.
    #[arg(long, env = "KOB_NODE_WRPC", default_value = "ws://127.0.0.1:18210")]
    pub rpc_url: String,
    /// Book snapshot (JSON, `MemoryBook`), re-read when it changes. Written by the indexer export.
    #[arg(long, env = "KOB_BOOK_FILE")]
    pub book_file: PathBuf,
    /// File holding the operator's secret key (64 hex, mode 0600). Not the key itself.
    #[arg(long, env = "KOB_OPERATOR_KEY_FILE")]
    pub key_file: Option<PathBuf>,
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
    /// The lowest fee rate, sompi per gram (≥ 100); with the node's fee estimate each transaction pays its urgency's bucket
    /// within `--fee-max-rate` and `--fee-max-tx-kas` (`docs/ops/executor.md`, fee policy).
    #[arg(long, default_value_t = 100)]
    pub fee_rate: u64,
    #[command(flatten)]
    pub fees: crate::fee::FeeArgs,
    /// Kill switch: nothing is built or submitted while this file exists.
    #[arg(long)]
    pub pause_file: Option<PathBuf>,
    /// Prometheus textfile-collector output (`.prom`).
    #[arg(long)]
    pub metrics_file: Option<PathBuf>,
    /// Warn (and set `kob_operator_low_funds`) below this many KAS of funding.
    #[arg(long, default_value_t = 100)]
    pub low_funds_kas: u64,
}

/// `kob-executor match`.
#[derive(Args, Debug, Clone)]
pub struct MatchArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Smallest profit per batch after its fee, sompi.
    #[arg(long, default_value_t = 1)]
    pub min_profit: i64,
    /// Optional cap on the size of one transaction, bytes. 0 (the default): the physical limit (250 000 bytes of transient
    /// mass and the block mass limits of the built transaction); a larger batch only costs its own fee.
    #[arg(long, default_value_t = 0)]
    pub max_tx_bytes: u64,
    /// Do not chain onto unaccepted parents (a partially filled order continues next tick).
    #[arg(long)]
    pub no_chain: bool,
    /// Do not arm or ratchet stops with updates in the batches (§4.3; the stops the batch's plain fills trigger are
    /// still filled next to their evidence).
    #[arg(long)]
    pub no_arm: bool,
    /// Wall-clock budget of one matcher tick in milliseconds (0: unlimited); the books left wait for the next tick.
    #[arg(long, default_value_t = 20_000)]
    pub tick_budget_ms: u64,
    /// Optional cap: best-priority candidates per (book, class, side) the planner looks at. 0 (the default): every candidate
    /// (the walks are sorted and the tick budget bounds a hostile book).
    #[arg(long, default_value_t = 0)]
    pub max_candidates_per_group: usize,
    /// The surplus-inventory policy (strict JSON, `docs/ops/executor.md`, "Surplus inventory"): the pair surpluses the
    /// operator may accumulate as inventory (never sold by maintenance). Default: none (off).
    #[arg(long)]
    pub inventory_policy: Option<PathBuf>,
    /// Also run the keepers in this process.
    #[arg(long)]
    pub keep: bool,
}

/// `kob-executor keep`.
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
    /// Keepers: return proven foreign strays (other tokens owned by an order id) to the maker inside the refund, kill or
    /// close that ends the order (permissionless; dropped from a job that would not pay with them).
    #[arg(long)]
    pub return_foreign_strays: bool,
    /// Smallest profit (tip − fee) per job, sompi.
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

fn run_cfg(c: &CommonArgs, roles: Roles) -> RunConfig {
    RunConfig {
        network: c.network.clone(),
        roles,
        dry_run: c.dry_run,
        tick: Duration::from_millis(c.tick_ms.max(100)),
        pause_file: c.pause_file.clone(),
        metrics_file: c.metrics_file.clone(),
        low_funds: c.low_funds_kas * 100_000_000,
        fee_policy: c.fees.policy(c.fee_rate).unwrap_or_else(|_| crate::fee::FeePolicy::fixed(c.fee_rate)),
        ..RunConfig::default()
    }
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

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

/// `kob-executor match`.
pub async fn run_match(a: MatchArgs) -> anyhow::Result<()> {
    a.common.fees.policy(a.common.fee_rate).map_err(anyhow::Error::msg)?;
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
    let mut cfg = run_cfg(&a.common, Roles { matcher: true, keeper: a.keep });
    cfg.engine = engine;
    cfg.keeper.fee_rate = a.common.fee_rate;
    cfg.maintenance.fee_rate = a.common.fee_rate;
    cfg.maintenance.inventory = inventory;
    let node = WrpcNode::new(WrpcConfig::new(a.common.rpc_url.clone()));
    tracing::info!(network = %cfg.network, rpc = %a.common.rpc_url, operator = %kob_protocol::json::to_hex(&signer.pubkey()), dry_run = cfg.dry_run, "matcher starting");
    let mut r = Runner::new(node, FileSource::new(a.common.book_file.clone()), signer, cfg);
    r.run(shutdown_signal()).await
}

/// `kob-executor keep`.
pub async fn run_keep(a: KeepArgs) -> anyhow::Result<()> {
    a.common.fees.policy(a.common.fee_rate).map_err(anyhow::Error::msg)?;
    let signer = load_key(&a.common)?;
    let mut cfg = run_cfg(&a.common, Roles { matcher: false, keeper: true });
    cfg.keeper = KeeperConfig {
        fee_rate: a.common.fee_rate,
        refund: !a.no_refund,
        sweep_own_strays: a.sweep_own_strays,
        return_foreign_strays: a.return_foreign_strays,
        min_profit: a.min_profit,
        ..KeeperConfig::default()
    };
    if a.common.offline {
        let book = read_snapshot(&a.common.book_file)?;
        let inp = crate::keepers::KeeperInput {
            orders: book.orders,
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
    let node = WrpcNode::new(WrpcConfig::new(a.common.rpc_url.clone()));
    tracing::info!(network = %cfg.network, rpc = %a.common.rpc_url, operator = %kob_protocol::json::to_hex(&signer.pubkey()), dry_run = cfg.dry_run, "keeper starting");
    let mut r = Runner::new(node, FileSource::new(a.common.book_file.clone()), signer, cfg);
    r.run(shutdown_signal()).await
}
