//! CPU backend: `kaspa_pow::State::check_pow` on N threads.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use crate::job::Job;

/// Search nonces on `threads` threads until one is found or `deadline`.
pub fn search(job: &Job, threads: usize, deadline: Instant, hashes: &AtomicU64) -> Option<u64> {
    let state = &job.state;
    let stop = AtomicBool::new(false);
    let found = AtomicU64::new(0);
    let seed = crate::random_seed();
    std::thread::scope(|s| {
        for t in 0..threads {
            let (stop, found) = (&stop, &found);
            s.spawn(move || {
                // Distinct pseudo-random starting point per thread (splitmix-style mix).
                let mut nonce = crate::mix64(seed ^ (t as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
                while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                    for _ in 0..2048u32 {
                        if state.check_pow(nonce).0 {
                            found.store(nonce, Ordering::SeqCst);
                            stop.store(true, Ordering::SeqCst);
                            hashes.fetch_add(1, Ordering::Relaxed);
                            return;
                        }
                        nonce = nonce.wrapping_add(1);
                    }
                    hashes.fetch_add(2048, Ordering::Relaxed);
                }
            });
        }
    });
    if stop.load(Ordering::SeqCst) { Some(found.load(Ordering::SeqCst)) } else { None }
}
