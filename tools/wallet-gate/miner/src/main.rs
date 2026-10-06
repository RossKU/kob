//! tn10-miner: standalone Kaspa testnet-10 miner (CPU threads or an OpenCL GPU), optionally intermittent. See README.md.
//!
//!   tn10-miner [--backend cpu|gpu|auto] [ADDR[,ADDR]...]     mine (pay addresses from the args or WALLET)
//!   tn10-miner balance ADDR...                              balance and spendable (coinbase-matured) KAS per address
//!   tn10-miner bench [cpu|gpu|gpu-sweep] [SECONDS]          raw hashrate on a synthetic header (no node)
//!   tn10-miner gpu-list                                     OpenCL GPU devices

use std::env;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use tn10_miner::job::{Job, synthetic_header};
use tn10_miner::rpc::Rpc;
use tn10_miner::run::{self, RunConfig, kas};
use tn10_miner::settings::{self, BackendKind, FileSettings};

const DEFAULT_NODE: &str = "ws://127.0.0.1:18210";

fn bench(args: &[String]) -> Result<()> {
    let what = args.first().map(String::as_str).unwrap_or("cpu");
    let secs: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10.0);
    let env = run::process_env();
    let path = settings::settings_path(&*env);
    let file = match &path {
        Some(p) => settings::load_file(p)?,
        None => FileSettings::default(),
    };
    let s = settings::resolve(&*env, &file, None)?;
    // bits 0x03000001: target 1, no nonce passes; pure throughput
    let job = Job::new(&synthetic_header(1, 0x0300_0001));
    match what {
        "cpu" => {
            let hashes = AtomicU64::new(0);
            let t0 = Instant::now();
            tn10_miner::cpu::search(&job, s.threads, t0 + Duration::from_secs_f64(secs), &hashes);
            let rate = hashes.load(Ordering::Relaxed) as f64 / t0.elapsed().as_secs_f64();
            println!("cpu threads={} {:.0} kH/s", s.threads, rate / 1e3);
        }
        #[cfg(feature = "gpu")]
        "gpu" => {
            let mut g = tn10_miner::gpu::Gpu::open(s.gpu.clone())?;
            let hashes = AtomicU64::new(0);
            // auto-tune (or the fixed global) exactly as mining does, then measure
            g.search(&job, Instant::now() + Duration::from_secs(2), &hashes)?;
            hashes.store(0, Ordering::Relaxed);
            let t0 = Instant::now();
            g.search(&job, t0 + Duration::from_secs_f64(secs), &hashes)?;
            let rate = hashes.load(Ordering::Relaxed) as f64 / t0.elapsed().as_secs_f64();
            println!(
                "gpu {} global={} local={} dispatch={:.1}ms {:.0} kH/s false_positives={}",
                g.device_name,
                g.global,
                s.gpu.local.map(|l| l.to_string()).unwrap_or_else(|| "driver".into()),
                g.dispatch_time.as_secs_f64() * 1e3,
                rate / 1e3,
                g.false_positives
            );
        }
        #[cfg(feature = "gpu")]
        "gpu-sweep" => {
            for local in [None, Some(64), Some(128), Some(256), Some(512)] {
                let mut g = tn10_miner::gpu::Gpu::open(settings::GpuOptions { local, ..s.gpu.clone() })?;
                for shift in [16, 18, 20, 22, 24] {
                    let global = 1usize << shift;
                    let rate = g.bench(&job, global, secs)?;
                    println!(
                        "gpu {} local={:>6} global=2^{shift} dispatch={:>7.2}ms {:>8.0} kH/s",
                        g.device_name,
                        local.map(|l| l.to_string()).unwrap_or_else(|| "driver".into()),
                        g.dispatch_time.as_secs_f64() * 1e3,
                        rate / 1e3
                    );
                }
            }
        }
        o => bail!("bench what? cpu{} (got {o:?})", if cfg!(feature = "gpu") { ", gpu, gpu-sweep" } else { "" }),
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let node = env::var("NODE").or_else(|_| env::var("KOB_TN10_WRPC")).unwrap_or_else(|_| DEFAULT_NODE.to_string());
    let mut args: Vec<String> = env::args().skip(1).collect();
    let mut cli_backend = None;
    if let Some(i) = args.iter().position(|a| a == "--backend") {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        cli_backend = Some(v.parse::<BackendKind>()?);
        args.drain(i..(i + 2).min(args.len()));
    }
    match args.first().map(String::as_str) {
        Some("bench") => return bench(&args[1..]),
        Some("gpu-list") => {
            #[cfg(feature = "gpu")]
            for (i, (p, _, d)) in tn10_miner::gpu::list_devices()?.into_iter().enumerate() {
                println!("{i}: {d} ({p})");
            }
            #[cfg(not(feature = "gpu"))]
            bail!("built without the gpu feature");
            #[cfg(feature = "gpu")]
            return Ok(());
        }
        _ => {}
    }
    let balance_mode = args.first().map(|a| a == "balance").unwrap_or(false);
    if balance_mode {
        args.remove(0);
    }
    let mut wallet_src = args.join(",");
    if wallet_src.is_empty() {
        wallet_src = env::var("WALLET").unwrap_or_default();
    }
    let wallets: Vec<String> =
        wallet_src.split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()).map(String::from).collect();
    if wallets.is_empty() {
        bail!("no pay address: set WALLET=addr[,addr...] or pass addresses as CLI args");
    }

    let env = run::process_env();
    let settings_file = settings::settings_path(&*env);
    let file = match &settings_file {
        Some(p) => settings::load_file(p)?,
        None => FileSettings::default(),
    };
    let s = settings::resolve(&*env, &file, cli_backend)?;

    if balance_mode {
        let mut rpc = Rpc::connect(&node).await?;
        for w in &wallets {
            let total = rpc.balance(w).await?;
            let b = rpc.spendable(std::slice::from_ref(w), s.maturity_daa).await?;
            println!(
                "{w} {} KAS ({total} sompi), spendable {} KAS, immature coinbase {} KAS, {} utxos",
                kas(total),
                kas(b.spendable),
                kas(b.immature),
                b.utxos
            );
        }
        return Ok(());
    }

    if let Some(p) = &settings_file {
        eprintln!("[miner] settings file {} (re-read on change)", p.display());
    }
    let max_blocks: u64 = env::var("MAX_BLOCKS").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let sum = run::run(RunConfig {
        node: node.clone(),
        wallets: wallets.clone(),
        max_blocks,
        settings: s.clone(),
        settings_file,
        env,
        cli_backend,
        report_every: Duration::from_secs(30),
        max_runtime: None,
    })
    .await?;
    if sum.verify_failures > 0 {
        eprintln!("[miner] WARNING: {} nonce(s) failed kaspa_pow verification (none was submitted)", sum.verify_failures);
    }
    if let Ok(mut rpc) = Rpc::connect(&node).await {
        let rpc = &mut rpc;
        for w in &wallets {
            if let Ok(b) = rpc.balance(w).await {
                eprintln!("[miner] balance {w} = {} KAS", kas(b));
            }
        }
    }
    Ok(())
}
