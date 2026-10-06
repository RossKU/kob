//! Miner settings: CLI flag > environment > settings file > default. The settings file (JSON) is `MINER_CONFIG`, else
//! `tn10-miner.json` next to the executable when it exists; the miner re-reads it when it changes (watermarks, duty cycle, poll,
//! threads, round time apply at once; backend and GPU options need a restart).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::throttle::ThrottleConfig;

pub const SOMPI_PER_KAS: f64 = 100_000_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
    Cpu,
    Gpu,
    /// the GPU when the build has it and a device opens, else the CPU
    Auto,
}

impl std::str::FromStr for BackendKind {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cpu" => Ok(Self::Cpu),
            "gpu" => Ok(Self::Gpu),
            "auto" => Ok(Self::Auto),
            o => bail!("backend must be cpu, gpu or auto (got {o:?})"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GpuOptions {
    /// device index (as `tn10-miner gpu-list` prints it) or a case-insensitive name substring; default: the first GPU
    pub device: Option<String>,
    /// fixed nonces per dispatch; default: auto-tuned so that one dispatch takes `dispatch_ms`
    pub global: Option<usize>,
    /// work-group size; default 128 (RTX 3060 Laptop sweep: 64 / 128 ~92 MH/s, driver choice ~86, 512 ~66)
    pub local: Option<usize>,
    pub dispatch_ms: u64,
}

impl Default for GpuOptions {
    fn default() -> Self {
        Self { device: None, global: None, local: Some(128), dispatch_ms: 60 }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GpuFile {
    pub device: Option<String>,
    pub global: Option<usize>,
    pub local: Option<usize>,
    pub dispatch_ms: Option<u64>,
}

/// The settings file. Every key is optional.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileSettings {
    pub backend: Option<String>,
    pub threads: Option<usize>,
    pub round_ms: Option<u64>,
    pub low_kas: Option<f64>,
    pub high_kas: Option<f64>,
    pub max_duty: Option<f64>,
    pub duty_window_sec: Option<u64>,
    pub poll_sec: Option<u64>,
    pub maturity_daa: Option<u64>,
    /// address(es) whose spendable balance the watermarks apply to (comma separated); default: the pay address(es)
    pub balance_address: Option<String>,
    pub gpu: Option<GpuFile>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub backend: BackendKind,
    pub threads: usize,
    pub round_ms: u64,
    pub throttle: ThrottleConfig,
    /// balance poll interval (idle and while mining)
    pub poll: Duration,
    pub maturity_daa: u64,
    pub balance_addresses: Option<Vec<String>>,
    pub gpu: GpuOptions,
}

pub fn default_threads() -> usize {
    num_cpus::get_physical().saturating_sub(1).max(1)
}

fn kas_to_sompi(name: &str, k: f64) -> Result<u64> {
    if !(0.0..=29e9).contains(&k) {
        bail!("{name} must be a KAS amount >= 0 (got {k})");
    }
    Ok((k * SOMPI_PER_KAS).round() as u64)
}

/// Resolve settings from `env` (a lookup, so tests need no process environment), the file and an optional CLI backend.
pub fn resolve(env: &dyn Fn(&str) -> Option<String>, file: &FileSettings, cli_backend: Option<BackendKind>) -> Result<Settings> {
    fn pick<T: std::str::FromStr>(env: &dyn Fn(&str) -> Option<String>, var: &str, file: Option<T>) -> Result<Option<T>>
    where
        T::Err: std::fmt::Display,
    {
        match env(var).filter(|s| !s.trim().is_empty()) {
            Some(s) => s.trim().parse::<T>().map(Some).map_err(|e| anyhow!("{var}={s:?}: {e}")),
            None => Ok(file),
        }
    }
    let backend = match cli_backend {
        Some(b) => b,
        None => match env("MINER_BACKEND").filter(|s| !s.trim().is_empty()).or(file.backend.clone()) {
            Some(s) => s.parse()?,
            None if cfg!(feature = "gpu") => BackendKind::Auto,
            None => BackendKind::Cpu,
        },
    };
    let threads = pick::<usize>(env, "THREADS", file.threads)?.filter(|&n| n > 0).unwrap_or_else(default_threads);
    let round_ms = pick::<u64>(env, "ROUND_MS", file.round_ms)?.unwrap_or(3000).max(100);
    let low = pick::<f64>(env, "MINER_LOW_KAS", file.low_kas)?;
    let high = pick::<f64>(env, "MINER_HIGH_KAS", file.high_kas)?;
    let watermarks = match (low, high) {
        (None, None) => None,
        (Some(l), Some(h)) => {
            let (l, h) = (kas_to_sompi("lowKas", l)?, kas_to_sompi("highKas", h)?);
            if l >= h {
                bail!("the low watermark must be below the high one (lowKas < highKas)");
            }
            Some((l, h))
        }
        _ => bail!("set both watermarks (lowKas and highKas / MINER_LOW_KAS and MINER_HIGH_KAS) or neither"),
    };
    let max_duty = pick::<f64>(env, "MINER_MAX_DUTY", file.max_duty)?.unwrap_or(1.0);
    if !(max_duty > 0.0 && max_duty <= 1.0) {
        bail!("maxDuty must be in (0, 1] (got {max_duty})");
    }
    let window = pick::<u64>(env, "MINER_DUTY_WINDOW_SEC", file.duty_window_sec)?.unwrap_or(600);
    if window == 0 {
        bail!("dutyWindowSec must be > 0");
    }
    let poll = pick::<u64>(env, "MINER_POLL_SEC", file.poll_sec)?.unwrap_or(15).max(1);
    let maturity_daa = pick::<u64>(env, "MINER_MATURITY_DAA", file.maturity_daa)?.unwrap_or(1000);
    let balance_addresses = env("MINER_BALANCE_ADDRESS")
        .filter(|s| !s.trim().is_empty())
        .or(file.balance_address.clone())
        .map(|s| s.split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty()).map(String::from).collect::<Vec<_>>())
        .filter(|v| !v.is_empty());
    let g = file.gpu.clone().unwrap_or_default();
    let d = GpuOptions::default();
    let gpu = GpuOptions {
        device: env("GPU_DEVICE").filter(|s| !s.trim().is_empty()).or(g.device),
        global: pick::<usize>(env, "GPU_GLOBAL", g.global)?.filter(|&n| n > 0),
        local: pick::<usize>(env, "GPU_LOCAL", g.local)?.filter(|&n| n > 0).or(d.local),
        dispatch_ms: pick::<u64>(env, "GPU_DISPATCH_MS", g.dispatch_ms)?.unwrap_or(d.dispatch_ms).max(1),
    };
    Ok(Settings {
        backend,
        threads,
        round_ms,
        throttle: ThrottleConfig { watermarks, max_duty, window: Duration::from_secs(window) },
        poll: Duration::from_secs(poll),
        maturity_daa,
        balance_addresses,
        gpu,
    })
}

/// The settings file in effect: `MINER_CONFIG`, else `tn10-miner.json` next to the executable if it exists.
pub fn settings_path(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(p) = env("MINER_CONFIG").filter(|s| !s.trim().is_empty()) {
        return Some(PathBuf::from(p));
    }
    let side = std::env::current_exe().ok()?.parent()?.join("tn10-miner.json");
    side.exists().then_some(side)
}

pub fn load_file(path: &Path) -> Result<FileSettings> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn defaults_are_continuous() {
        let s = resolve(&env_of(&[]), &FileSettings::default(), None).unwrap();
        assert_eq!(s.throttle, ThrottleConfig::default());
        assert_eq!(s.round_ms, 3000);
        assert_eq!(s.poll, Duration::from_secs(15));
        assert_eq!(s.backend, if cfg!(feature = "gpu") { BackendKind::Auto } else { BackendKind::Cpu });
    }

    #[test]
    fn file_then_env_then_cli() {
        let file: FileSettings = serde_json::from_str(
            r#"{"backend":"gpu","threads":4,"lowKas":1000,"highKas":5000.5,"maxDuty":0.5,"dutyWindowSec":300,"pollSec":10,
                "gpu":{"device":"RTX","dispatchMs":40}}"#,
        )
        .unwrap();
        let s = resolve(&env_of(&[]), &file, None).unwrap();
        assert_eq!(s.backend, BackendKind::Gpu);
        assert_eq!(s.threads, 4);
        assert_eq!(s.throttle.watermarks, Some((100_000_000_000, 500_050_000_000)));
        assert_eq!(s.throttle.max_duty, 0.5);
        assert_eq!(s.throttle.window, Duration::from_secs(300));
        assert_eq!(s.gpu.device.as_deref(), Some("RTX"));
        assert_eq!(s.gpu.dispatch_ms, 40);
        let s = resolve(&env_of(&[("MINER_BACKEND", "cpu"), ("THREADS", "12"), ("MINER_LOW_KAS", "10")]), &file, None).unwrap();
        assert_eq!((s.backend, s.threads), (BackendKind::Cpu, 12));
        assert_eq!(s.throttle.watermarks, Some((1_000_000_000, 500_050_000_000)));
        let s = resolve(&env_of(&[("MINER_BACKEND", "cpu")]), &file, Some(BackendKind::Auto)).unwrap();
        assert_eq!(s.backend, BackendKind::Auto);
    }

    #[test]
    fn rejects_bad_settings() {
        let f = |j: &str| serde_json::from_str::<FileSettings>(j).unwrap();
        let e = |j: &str| resolve(&env_of(&[]), &f(j), None).unwrap_err().to_string();
        assert!(e(r#"{"lowKas":10}"#).contains("both watermarks"));
        assert!(e(r#"{"lowKas":10,"highKas":10}"#).contains("below the high"));
        assert!(e(r#"{"maxDuty":0}"#).contains("maxDuty"));
        assert!(e(r#"{"maxDuty":1.5}"#).contains("maxDuty"));
        assert!(e(r#"{"backend":"tpu"}"#).contains("cpu, gpu or auto"));
        assert!(serde_json::from_str::<FileSettings>(r#"{"lowkas":1}"#).is_err(), "unknown keys are refused");
        assert!(resolve(&env_of(&[("THREADS", "x")]), &FileSettings::default(), None).is_err());
    }
}
