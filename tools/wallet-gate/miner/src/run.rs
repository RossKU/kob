//! The mining loop: getBlockTemplate -> nonce search (CPU or GPU) -> kaspa_pow verification -> submitBlock, gated by the throttle.
//!
//! The block is kept as the raw JSON the node returned (never round-tripped through typed transactions) and only `header.nonce` is
//! replaced, so tx version, compute budget, covenant fields, gas etc. are exactly what the node produced.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use kaspa_consensus_core::header::Header;
use kaspa_rpc_core::{GetBlockTemplateRequest, RpcAddress, RpcRawHeader};
use serde_json::{Value, json};

use crate::cpu;
use crate::job::Job;
use crate::poller::{PollCtl, Poller};
use crate::rpc::{Balance, Rpc};
use crate::settings::{self, BackendKind, GpuOptions, Settings};
use crate::throttle::{Reason, Throttle};

pub fn kas(sompi: u64) -> String {
    format!("{}.{:08}", sompi / 100_000_000, sompi % 100_000_000)
}

fn kas_short(sompi: u64) -> String {
    format!("{:.1}", sompi as f64 / 1e8)
}

pub enum Backend {
    Cpu,
    #[cfg(feature = "gpu")]
    Gpu(Box<crate::gpu::Gpu>),
}

impl Backend {
    pub fn open(kind: BackendKind, gpu: &GpuOptions) -> Result<Self> {
        let _ = gpu;
        match kind {
            BackendKind::Cpu => Ok(Self::Cpu),
            #[cfg(feature = "gpu")]
            BackendKind::Gpu => Ok(Self::Gpu(Box::new(crate::gpu::Gpu::open(gpu.clone())?))),
            #[cfg(not(feature = "gpu"))]
            BackendKind::Gpu => bail!("this tn10-miner was built without the gpu feature (cargo build --release --features gpu)"),
            #[cfg(feature = "gpu")]
            BackendKind::Auto => match crate::gpu::Gpu::open(gpu.clone()) {
                Ok(g) => Ok(Self::Gpu(Box::new(g))),
                Err(e) => {
                    eprintln!("[miner] backend auto: no usable OpenCL GPU ({e:#}); mining on the CPU");
                    Ok(Self::Cpu)
                }
            },
            #[cfg(not(feature = "gpu"))]
            BackendKind::Auto => Ok(Self::Cpu),
        }
    }

    pub fn describe(&self, threads: usize) -> String {
        match self {
            Self::Cpu => format!("cpu ({threads} threads)"),
            #[cfg(feature = "gpu")]
            Self::Gpu(g) => format!("gpu ({}, opencl)", g.device_name),
        }
    }

    pub fn is_gpu(&self) -> bool {
        !matches!(self, Self::Cpu)
    }

    pub fn search(&mut self, job: &Job, threads: usize, deadline: Instant, hashes: &AtomicU64) -> Result<Option<u64>> {
        match self {
            Self::Cpu => Ok(cpu::search(job, threads, deadline, hashes)),
            #[cfg(feature = "gpu")]
            Self::Gpu(g) => g.search(job, deadline, hashes),
        }
    }

    pub fn false_positives(&self) -> u64 {
        match self {
            Self::Cpu => 0,
            #[cfg(feature = "gpu")]
            Self::Gpu(g) => g.false_positives,
        }
    }
}

pub type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

pub fn process_env() -> EnvLookup {
    Arc::new(|k| std::env::var(k).ok())
}

pub struct RunConfig {
    pub node: String,
    pub wallets: Vec<String>,
    /// stop after this many accepted blocks (0 = unlimited)
    pub max_blocks: u64,
    pub settings: Settings,
    /// re-read on change (hot settings)
    pub settings_file: Option<PathBuf>,
    pub env: EnvLookup,
    pub cli_backend: Option<BackendKind>,
    pub report_every: Duration,
    /// stop after this long (tests); None = run until max_blocks
    pub max_runtime: Option<Duration>,
}

#[derive(Debug, Default, Clone)]
pub struct Summary {
    pub backend: String,
    pub accepted: u64,
    pub rejected: u64,
    pub hashes: u64,
    /// wall time spent searching
    pub mining: Duration,
    pub elapsed: Duration,
    /// nonces a backend reported that kaspa_pow then rejected (never submitted)
    pub verify_failures: u64,
    pub bursts: u64,
    pub templates: u64,
}

fn mtime(p: &PathBuf) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn reason_text(r: Reason) -> &'static str {
    match r {
        Reason::Continuous => "continuous",
        Reason::Burst => "burst (spendable below the low watermark)",
        Reason::BalanceOk => "spendable at or above the watermarks",
        Reason::DutyCap => "duty-cycle cap",
        Reason::Unknown => "balance not read yet",
    }
}

pub async fn run(cfg: RunConfig) -> Result<Summary> {
    let addrs: Vec<RpcAddress> = cfg
        .wallets
        .iter()
        .map(|w| RpcAddress::try_from(w.as_str()).map_err(|e| anyhow!("bad address {w}: {e:?}")))
        .collect::<Result<_>>()?;
    if addrs.is_empty() {
        bail!("no pay address");
    }
    let mut s = cfg.settings.clone();
    let mut backend = Backend::open(s.backend, &s.gpu)?;
    let mut sum = Summary { backend: backend.describe(s.threads), ..Default::default() };
    let watch = |s: &Settings| s.balance_addresses.clone().unwrap_or_else(|| cfg.wallets.clone());
    eprintln!(
        "[miner] node={} backend={} round_ms={} max_blocks={} pay={} address(es){}",
        cfg.node,
        sum.backend,
        s.round_ms,
        if cfg.max_blocks == 0 { "unlimited".to_string() } else { cfg.max_blocks.to_string() },
        cfg.wallets.len(),
        throttle_text(&s),
    );

    let mut rpc = Rpc::connect(&cfg.node).await?;
    let mut throttle = Throttle::new(s.throttle.clone());
    let mut stamp = cfg.settings_file.as_ref().and_then(mtime);
    let hashes = AtomicU64::new(0);
    let start = Instant::now();
    let mut errs = 0u32;
    let mut turn = 0usize;
    let mut bal: Option<Balance> = None;
    let mut poller: Option<Poller> = None;
    let mut last_mined: Option<Instant> = None;
    let mut last_report = Instant::now();
    let (mut rep_hashes, mut rep_mining) = (0u64, Duration::ZERO);
    let mut state: Option<(bool, Reason)> = None;

    loop {
        if cfg.max_blocks > 0 && sum.accepted >= cfg.max_blocks {
            break;
        }
        if cfg.max_runtime.is_some_and(|m| start.elapsed() >= m) {
            break;
        }

        // hot settings
        if let Some(p) = &cfg.settings_file {
            let now_stamp = mtime(p);
            if now_stamp != stamp {
                stamp = now_stamp;
                match settings::load_file(p).and_then(|f| settings::resolve(&*cfg.env, &f, cfg.cli_backend)) {
                    Ok(n) => {
                        if n.backend != s.backend || n.gpu != s.gpu {
                            eprintln!("[miner] settings: backend / gpu changes take effect on the next start");
                        }
                        let (backend_kind, gpu) = (s.backend, s.gpu.clone());
                        s = Settings { backend: backend_kind, gpu, ..n };
                        throttle.cfg = s.throttle.clone();
                        if let Some(p) = &poller {
                            p.update(|c| {
                                c.addrs = watch(&s);
                                c.maturity_daa = s.maturity_daa;
                                c.poll = s.poll;
                            });
                            p.kick();
                        }
                        eprintln!("[miner] settings reloaded from {}:{}", p.display(), throttle_text(&s));
                    }
                    Err(e) => eprintln!("[miner] settings file ignored (keeping the current settings): {e:#}"),
                }
            }
        }

        // balance (only with watermarks)
        // (a background poller on its own connection: a full UTXO listing can take ~30 s and must not stall the search)
        if throttle.cfg.watermarks.is_none() {
            poller = None;
        }
        if throttle.cfg.watermarks.is_some() && poller.is_none() {
            let ctl = PollCtl { addrs: watch(&s), maturity_daa: s.maturity_daa, poll: s.poll, recent: false };
            poller = Some(Poller::spawn(cfg.node.clone(), ctl));
        }
        if let Some(p) = poller.as_mut() {
            // our coinbase can be immature while mining and for twice the maturity time after (TN10: 10 DAA/s)
            let quiet = Duration::from_secs_f64(s.maturity_daa as f64 / 10.0 * 2.0).max(Duration::from_secs(60));
            let recent = throttle.is_mining() || last_mined.is_some_and(|t| t.elapsed() < quiet);
            p.update(|c| c.recent = recent);
            if let Some(b) = p.latest() {
                bal = Some(b);
                if throttle.observe_balance(b.spendable) == Some(true) {
                    sum.bursts += 1;
                }
            }
        }

        let now = Instant::now();
        let decision = throttle.decide(now);
        if state != Some(decision) {
            let b = bal
                .map(|b| match b.listed {
                    true => {
                        format!(" spendable={} KAS immature={} KAS ({} utxos)", kas_short(b.spendable), kas_short(b.immature), b.utxos)
                    }
                    false => format!(" spendable={} KAS (total)", kas_short(b.spendable)),
                })
                .unwrap_or_default();
            eprintln!(
                "[miner] {}: {}{b} duty={:.0}%",
                if decision.0 { "mining" } else { "idle" },
                reason_text(decision.1),
                throttle.duty(now) * 100.0
            );
            state = Some(decision);
        }
        if throttle.is_mining() && !decision.0 {
            last_mined = Some(now);
        }
        throttle.set_mining(now, decision.0);
        if !decision.0 {
            if last_report.elapsed() >= cfg.report_every * 4 {
                let b = bal.map(|b| format!(" spendable={} KAS", kas_short(b.spendable))).unwrap_or_default();
                eprintln!("[miner] idle{b} accepted={} duty={:.0}%", sum.accepted, throttle.duty(now) * 100.0);
                last_report = Instant::now();
                (rep_hashes, rep_mining) = (hashes.load(Ordering::Relaxed), sum.mining);
            }
            tokio::time::sleep(Duration::from_millis(250).min(s.poll)).await;
            continue;
        }

        let idx = turn % addrs.len();
        let req = serde_json::to_value(GetBlockTemplateRequest::new(addrs[idx].clone(), vec![]))?;
        let tpl = match rpc.call("getBlockTemplate", req).await {
            Ok(v) => {
                errs = 0;
                v
            }
            Err(e) => {
                errs += 1;
                eprintln!("[miner] getBlockTemplate error ({errs}): {e}; reconnecting");
                if errs > 30 {
                    bail!("too many consecutive errors");
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
                if let Ok(r) = Rpc::connect(&cfg.node).await {
                    rpc = r;
                }
                continue;
            }
        };
        sum.templates += 1;
        if tpl.get("isSynced").and_then(Value::as_bool) == Some(false) {
            eprintln!("[miner] warning: node reports not synced");
        }
        let mut block = tpl.get("block").cloned().ok_or_else(|| anyhow!("template has no block: {tpl}"))?;
        let raw_header: RpcRawHeader = serde_json::from_value(block["header"].clone()).context("parse header")?;
        let header: Header = (&raw_header).try_into().map_err(|e| anyhow!("header convert: {e:?}"))?;
        let job = Job::new(&header);

        let t0 = Instant::now();
        let mut deadline = t0 + Duration::from_millis(s.round_ms);
        if let Some(b) = throttle.duty_budget(t0) {
            deadline = deadline.min(t0 + b);
        }
        let threads = s.threads;
        let found = tokio::task::block_in_place(|| backend.search(&job, threads, deadline, &hashes));
        sum.mining += t0.elapsed();
        let found = match found {
            Ok(f) => f,
            Err(e) if backend.is_gpu() && s.backend == BackendKind::Auto => {
                eprintln!("[miner] GPU search failed ({e:#}); backend auto falls back to the CPU");
                backend = Backend::Cpu;
                sum.backend = backend.describe(s.threads);
                None
            }
            Err(e) => return Err(e.context("nonce search")),
        };

        if last_report.elapsed() >= cfg.report_every {
            let (h, m) = (hashes.load(Ordering::Relaxed), sum.mining);
            let secs = (m - rep_mining).as_secs_f64().max(1e-3);
            let b = bal.map(|b| format!(" spendable={} KAS", kas_short(b.spendable))).unwrap_or_default();
            eprintln!(
                "[miner] {:.0} kH/s, accepted={} elapsed={:.0}s backend={}{b} duty={:.0}%",
                (h - rep_hashes) as f64 / secs / 1e3,
                sum.accepted,
                start.elapsed().as_secs_f64(),
                sum.backend,
                throttle.duty(Instant::now()) * 100.0
            );
            last_report = Instant::now();
            (rep_hashes, rep_mining) = (h, m);
        }
        let Some(nonce) = found else { continue };
        // every nonce of every backend passes kaspa_pow on the CPU before it is submitted
        if !job.verify(nonce) {
            sum.verify_failures += 1;
            eprintln!("[miner] nonce {nonce:#018x} failed kaspa_pow verification; not submitted (total {})", sum.verify_failures);
            continue;
        }

        block["header"]["nonce"] = json!(nonce);
        let submit = match rpc.call("submitBlock", json!({ "block": block, "allowNonDAABlocks": false })).await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[miner] submitBlock error: {e}");
                continue;
            }
        };
        let ok = submit.get("report").and_then(|r| r.get("type")).and_then(Value::as_str) == Some("success");
        if ok {
            sum.accepted += 1;
            turn += 1;
            let mut hdr = raw_header.clone();
            hdr.nonce = nonce;
            let hash = Header::try_from(&hdr).map(|h| h.hash.to_string()).unwrap_or_default();
            println!(
                "[miner] block #{} accepted hash={hash} daa={} pay={} t={:.0}s",
                sum.accepted,
                raw_header.daa_score,
                cfg.wallets[idx],
                start.elapsed().as_secs_f64()
            );
        } else {
            sum.rejected += 1;
            eprintln!("[miner] block rejected (total {}): {submit}", sum.rejected);
        }
    }

    sum.hashes = hashes.load(Ordering::Relaxed);
    sum.elapsed = start.elapsed();
    sum.verify_failures += backend.false_positives();
    eprintln!(
        "[miner] done: accepted={} rejected={} elapsed={:.1}s mining={:.1}s avg={:.0} kH/s while mining, bursts={} backend={}",
        sum.accepted,
        sum.rejected,
        sum.elapsed.as_secs_f64(),
        sum.mining.as_secs_f64(),
        sum.hashes as f64 / sum.mining.as_secs_f64().max(1e-3) / 1e3,
        sum.bursts,
        sum.backend
    );
    Ok(sum)
}

fn throttle_text(s: &Settings) -> String {
    let mut t = String::new();
    match s.throttle.watermarks {
        Some((l, h)) => t += &format!(" intermittent: low={} KAS high={} KAS poll={}s", kas_short(l), kas_short(h), s.poll.as_secs()),
        None => t += " continuous",
    }
    if s.throttle.max_duty < 1.0 {
        t += &format!(" max_duty={:.0}% over {}s", s.throttle.max_duty * 100.0, s.throttle.window.as_secs());
    }
    t
}
