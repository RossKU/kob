//! tn10-miner: a standalone Kaspa testnet-10 miner that funds test addresses (the KOB soak's bank).
//!
//! Backends: CPU threads running `kaspa_pow::State::check_pow`, or (feature `gpu`) the OpenCL kHeavyHash kernel in
//! `kernels/kheavyhash.cl`; every nonce a backend reports is re-verified with kaspa_pow before submitBlock. Optional intermittent
//! mining (`throttle`): bursts only while the watched spendable balance is below a low watermark, up to a high one, under a
//! duty-cycle cap.

pub mod cpu;
#[cfg(feature = "gpu")]
pub mod gpu;
pub mod job;
pub mod poller;
pub mod rpc;
pub mod run;
pub mod settings;
pub mod throttle;

/// splitmix64 finalizer
pub fn mix64(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 33;
    x = x.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    x ^ (x >> 33)
}

/// A fresh seed for the nonce search start (time, a counter, the stack address).
pub fn random_seed() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    let local = 0u8;
    mix64(t ^ N.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed) ^ (&local as *const u8 as u64))
}
