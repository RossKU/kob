//! OpenCL backend (feature `gpu`): the kHeavyHash kernel of `kernels/kheavyhash.cl` over the nonce space, host data from `Job`
//! (rusty-kaspa), every reported nonce re-verified with kaspa_pow before it leaves this module.

use std::fmt::Display;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use opencl3::command_queue::CommandQueue;
use opencl3::context::Context;
use opencl3::device::{CL_DEVICE_TYPE_GPU, Device};
use opencl3::event::Event;
use opencl3::kernel::{ExecuteKernel, Kernel};
use opencl3::memory::{Buffer, CL_MEM_READ_ONLY, CL_MEM_READ_WRITE, CL_MEM_WRITE_ONLY};
use opencl3::platform::get_platforms;
use opencl3::program::Program;
use opencl3::types::{CL_BLOCKING, CL_NON_BLOCKING, cl_device_id, cl_uint, cl_ulong};

use crate::job::{HEAVY_INIT, Job, POW_INIT};
pub use crate::settings::GpuOptions;

pub const KERNEL_SOURCE: &str = include_str!("../kernels/kheavyhash.cl");
/// must match MAX_RESULTS in the kernel
const MAX_RESULTS: usize = 16;
const MIN_GLOBAL: usize = 1 << 14;
const MAX_GLOBAL: usize = 1 << 26;

fn cl<T, E: Display>(r: std::result::Result<T, E>, what: &str) -> Result<T> {
    r.map_err(|e| anyhow!("OpenCL {what}: {e}"))
}

/// Every OpenCL GPU device of every platform: (platform name, device id, device name).
pub fn list_devices() -> Result<Vec<(String, cl_device_id, String)>> {
    let mut out = Vec::new();
    for p in cl(get_platforms(), "platforms (is an OpenCL driver installed?)")? {
        let pname = p.name().unwrap_or_default();
        for id in p.get_devices(CL_DEVICE_TYPE_GPU).unwrap_or_default() {
            out.push((pname.clone(), id, Device::new(id).name().unwrap_or_default()));
        }
    }
    Ok(out)
}

pub struct Gpu {
    pub device_name: String,
    queue: CommandQueue,
    mine: Kernel,
    hash: Kernel,
    _program: Program,
    pow_init: Buffer<cl_ulong>,
    heavy_init: Buffer<cl_ulong>,
    pre_pow: Buffer<cl_ulong>,
    matrix: Buffer<u8>,
    target: Buffer<cl_ulong>,
    nonces: Buffer<cl_ulong>,
    count: Buffer<cl_uint>,
    // the context must outlive the buffers and the queue (dropped last)
    context: Context,
    opts: GpuOptions,
    /// nonces per dispatch (current; auto-tuned unless fixed)
    pub global: usize,
    granule: usize,
    /// smoothed wall time of one dispatch
    pub dispatch_time: Duration,
    /// nonces the kernel reported that kaspa_pow rejected (must stay 0)
    pub false_positives: u64,
}

impl Gpu {
    pub fn open(opts: GpuOptions) -> Result<Self> {
        let devices = list_devices()?;
        if devices.is_empty() {
            bail!("no OpenCL GPU device");
        }
        let (_, id, name) = match &opts.device {
            None => devices[0].clone(),
            Some(sel) => match sel.parse::<usize>() {
                Ok(i) => devices.get(i).cloned().ok_or_else(|| anyhow!("no OpenCL GPU #{i} ({} found)", devices.len()))?,
                Err(_) => devices
                    .iter()
                    .find(|d| d.2.to_lowercase().contains(&sel.to_lowercase()))
                    .cloned()
                    .ok_or_else(|| anyhow!("no OpenCL GPU named like {sel:?}"))?,
            },
        };
        let device = Device::new(id);
        let context = cl(Context::from_device(&device), "context")?;
        let queue = cl(CommandQueue::create_default_with_properties(&context, 0, 0), "queue")?;
        let program = Program::create_and_build_from_source(&context, KERNEL_SOURCE, "")
            .map_err(|e| anyhow!("OpenCL kernel build failed: {e}"))?;
        let mine = cl(Kernel::create(&program, "mine_nonces"), "kernel mine_nonces")?;
        let hash = cl(Kernel::create(&program, "hash_nonces"), "kernel hash_nonces")?;
        // SAFETY: plain device allocations without a host pointer
        let (mut pow_init, mut heavy_init, pre_pow, matrix, target, nonces, count) = unsafe {
            (
                cl(Buffer::<cl_ulong>::create(&context, CL_MEM_READ_ONLY, 25, ptr::null_mut()), "buffer")?,
                cl(Buffer::<cl_ulong>::create(&context, CL_MEM_READ_ONLY, 25, ptr::null_mut()), "buffer")?,
                cl(Buffer::<cl_ulong>::create(&context, CL_MEM_READ_ONLY, 4, ptr::null_mut()), "buffer")?,
                cl(Buffer::<u8>::create(&context, CL_MEM_READ_ONLY, 64 * 64, ptr::null_mut()), "buffer")?,
                cl(Buffer::<cl_ulong>::create(&context, CL_MEM_READ_ONLY, 4, ptr::null_mut()), "buffer")?,
                cl(Buffer::<cl_ulong>::create(&context, CL_MEM_READ_WRITE, MAX_RESULTS, ptr::null_mut()), "buffer")?,
                cl(Buffer::<cl_uint>::create(&context, CL_MEM_READ_WRITE, 1, ptr::null_mut()), "buffer")?,
            )
        };
        // SAFETY: blocking writes from live slices of the buffers' element type and length
        unsafe {
            cl(queue.enqueue_write_buffer(&mut pow_init, CL_BLOCKING, 0, &POW_INIT, &[]), "write")?;
            cl(queue.enqueue_write_buffer(&mut heavy_init, CL_BLOCKING, 0, &HEAVY_INIT, &[]), "write")?;
        }
        let granule = opts.local.unwrap_or(256).max(1);
        let global = round_to(opts.global.unwrap_or(1 << 20), granule);
        Ok(Self {
            device_name: name,
            queue,
            mine,
            hash,
            _program: program,
            pow_init,
            heavy_init,
            pre_pow,
            matrix,
            target,
            nonces,
            count,
            context,
            opts,
            global,
            granule,
            dispatch_time: Duration::from_millis(0),
            false_positives: 0,
        })
    }

    fn load(&mut self, job: &Job) -> Result<()> {
        // SAFETY: blocking writes from live slices of the buffers' element type and length
        unsafe {
            cl(self.queue.enqueue_write_buffer(&mut self.pre_pow, CL_BLOCKING, 0, &job.pre_pow, &[]), "write")?;
            cl(self.queue.enqueue_write_buffer(&mut self.matrix, CL_BLOCKING, 0, &job.matrix, &[]), "write")?;
            cl(self.queue.enqueue_write_buffer(&mut self.target, CL_BLOCKING, 0, &job.target, &[]), "write")?;
        }
        Ok(())
    }

    /// NVIDIA's OpenCL busy-spins a CPU core inside every blocking wait. Sleep through most of the expected kernel time, then poll
    /// the event with short sleeps, so the host thread costs next to no CPU while the GPU works.
    fn wait_quietly(&self, ev: &Event) -> Result<()> {
        let expect = self.dispatch_time.mul_f64(0.85);
        if expect > Duration::from_millis(1) {
            std::thread::sleep(expect);
        }
        loop {
            let st = cl(ev.command_execution_status(), "event status")?.0;
            if st == 0 {
                return Ok(()); // CL_COMPLETE
            }
            if st < 0 {
                bail!("OpenCL kernel failed (status {st})");
            }
            std::thread::sleep(Duration::from_micros(500));
        }
    }

    /// One dispatch of `global` nonces from `base`; the nonces the kernel reports (unverified).
    fn dispatch(&mut self, timestamp: u64, base: u64, global: usize) -> Result<Vec<u64>> {
        let t0 = Instant::now();
        let zero: [cl_uint; 1] = [0];
        // SAFETY: the kernel arguments match `mine_nonces` in type and order; `zero` outlives the write (blocking read below, same
        // in-order queue)
        let ev = unsafe {
            cl(self.queue.enqueue_write_buffer(&mut self.count, CL_NON_BLOCKING, 0, &zero, &[]), "write")?;
            let mut k = ExecuteKernel::new(&self.mine);
            k.set_arg(&self.pow_init)
                .set_arg(&self.heavy_init)
                .set_arg(&self.pre_pow)
                .set_arg(&timestamp)
                .set_arg(&self.matrix)
                .set_arg(&self.target)
                .set_arg(&base)
                .set_arg(&self.nonces)
                .set_arg(&self.count)
                .set_global_work_size(global);
            if let Some(l) = self.opts.local {
                k.set_local_work_size(l);
            }
            cl(k.enqueue_nd_range(&self.queue), "enqueue mine_nonces")?
        };
        cl(self.queue.flush(), "flush")?;
        self.wait_quietly(&ev)?;
        let mut count: [cl_uint; 1] = [0];
        // SAFETY: blocking reads into live slices of the buffers' element type
        unsafe { cl(self.queue.enqueue_read_buffer(&self.count, CL_BLOCKING, 0, &mut count, &[]), "read")? };
        let n = (count[0] as usize).min(MAX_RESULTS);
        let mut found = vec![0u64; n];
        if n > 0 {
            unsafe { cl(self.queue.enqueue_read_buffer(&self.nonces, CL_BLOCKING, 0, &mut found, &[]), "read")? };
        }
        let dt = t0.elapsed();
        self.dispatch_time = if self.dispatch_time.is_zero() { dt } else { self.dispatch_time.mul_f64(0.7) + dt.mul_f64(0.3) };
        Ok(found)
    }

    fn retune(&mut self, last: Duration) {
        if self.opts.global.is_some() || last.is_zero() {
            return;
        }
        let want = Duration::from_millis(self.opts.dispatch_ms.max(1));
        let ratio = (want.as_secs_f64() / last.as_secs_f64()).clamp(0.5, 2.0);
        let g = (self.global as f64 * ratio) as usize;
        self.global = round_to(g.clamp(MIN_GLOBAL, MAX_GLOBAL), self.granule);
    }

    /// Search until a nonce kaspa_pow accepts is found, or `deadline` (checked between dispatches).
    pub fn search(&mut self, job: &Job, deadline: Instant, hashes: &AtomicU64) -> Result<Option<u64>> {
        self.load(job)?;
        let mut base = crate::mix64(crate::random_seed());
        while Instant::now() < deadline {
            let global = self.global;
            let t0 = Instant::now();
            let found = self.dispatch(job.timestamp, base, global)?;
            self.retune(t0.elapsed());
            // the kernel stops early once a winner is known; work-items run roughly in global-id order, so the furthest winner is a fair
            // count of the nonces actually tried (the full dispatch would overstate the rate on easy targets)
            let tried = found.iter().map(|n| n.wrapping_sub(base) + 1).max().unwrap_or(global as u64);
            hashes.fetch_add(tried.min(global as u64), Ordering::Relaxed);
            base = base.wrapping_add(global as u64);
            for n in found {
                if job.verify(n) {
                    return Ok(Some(n));
                }
                self.false_positives += 1;
                eprintln!("[miner] GPU nonce {n:#018x} REJECTED by kaspa_pow (false positives so far: {})", self.false_positives);
            }
        }
        Ok(None)
    }

    /// The kernel's pow value of `n` nonces from `base` (test kernel `hash_nonces`, same code path as mining).
    pub fn hash_batch(&mut self, job: &Job, base: u64, n: usize) -> Result<Vec<[u64; 4]>> {
        self.load(job)?;
        // SAFETY: device allocation without host pointer; kernel arguments match `hash_nonces`; blocking read into a slice of the
        // buffer's exact length
        let mut flat = vec![0u64; n * 4];
        unsafe {
            let out = cl(Buffer::<cl_ulong>::create(&self.context, CL_MEM_WRITE_ONLY, n * 4, ptr::null_mut()), "buffer")?;
            let ts = job.timestamp;
            cl(
                ExecuteKernel::new(&self.hash)
                    .set_arg(&self.pow_init)
                    .set_arg(&self.heavy_init)
                    .set_arg(&self.pre_pow)
                    .set_arg(&ts)
                    .set_arg(&self.matrix)
                    .set_arg(&base)
                    .set_arg(&out)
                    .set_global_work_size(n)
                    .enqueue_nd_range(&self.queue),
                "enqueue hash_nonces",
            )?;
            cl(self.queue.enqueue_read_buffer(&out, CL_BLOCKING, 0, &mut flat, &[]), "read")?;
        }
        Ok(flat.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect())
    }

    /// Raw kernel throughput: `secs` of back-to-back dispatches at a fixed `global` against an unreachable target; hashes per second.
    pub fn bench(&mut self, job: &Job, global: usize, secs: f64) -> Result<f64> {
        self.load(job)?;
        let mut base = 0u64;
        let mut done = 0u64;
        // one warm-up dispatch (first-launch costs, a dispatch-time estimate for the quiet wait)
        self.dispatch_time = Duration::ZERO;
        self.dispatch(job.timestamp, base, global)?;
        let t0 = Instant::now();
        while t0.elapsed().as_secs_f64() < secs {
            base = base.wrapping_add(global as u64);
            let found = self.dispatch(job.timestamp, base, global)?;
            done += global as u64;
            for n in found {
                if !job.verify(n) {
                    self.false_positives += 1;
                }
            }
        }
        Ok(done as f64 / t0.elapsed().as_secs_f64())
    }
}

fn round_to(x: usize, g: usize) -> usize {
    x.div_ceil(g).max(1) * g
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::tests::{header, le_leq};

    /// Runs where an OpenCL GPU exists; prints why it is skipped elsewhere (CI). Set TN10_MINER_REQUIRE_GPU=1 to make a missing
    /// device a failure instead.
    fn gpu() -> Option<Gpu> {
        match Gpu::open(GpuOptions::default()) {
            Ok(g) => {
                eprintln!("GPU test device: {}", g.device_name);
                Some(g)
            }
            Err(e) => {
                assert!(std::env::var("TN10_MINER_REQUIRE_GPU").is_err(), "GPU required but unavailable: {e:#}");
                eprintln!("skipping GPU test: {e:#}");
                None
            }
        }
    }

    #[test]
    fn kernel_equals_kaspa_pow_on_fixed_headers() {
        let Some(mut g) = gpu() else { return };
        for seed in [0u8, 1, 7, 42, 200, 255] {
            let job = Job::new(&header(seed, 0x1e7fffff));
            for base in [0u64, 0xFFFF_FFFF_FFFF_FF00, 0x1234_5678_9ABC_DEF0 ^ ((seed as u64) << 40)] {
                let got = g.hash_batch(&job, base, 4096).unwrap();
                for (i, h) in got.iter().enumerate() {
                    let nonce = base.wrapping_add(i as u64);
                    assert_eq!(*h, job.pow(nonce), "seed {seed} nonce {nonce:#x}");
                }
            }
        }
    }

    #[test]
    fn kernel_finds_exactly_what_kaspa_pow_accepts() {
        let Some(mut g) = gpu() else { return };
        // ~1 in 4096 nonces pass: compare the kernel's winners of one dispatch with an exhaustive kaspa_pow scan of the same range
        let job = Job::new(&header(77, 0x1f0fffff));
        let base = 5_000_000u64;
        let n = 1usize << 16;
        let expect: Vec<u64> = (0..n as u64).map(|i| base + i).filter(|&x| job.verify(x)).collect();
        assert!(!expect.is_empty());
        let h = g.hash_batch(&job, base, n).unwrap();
        let kernel_pass: Vec<u64> = (0..n).filter(|&i| le_leq(&h[i], &job.target)).map(|i| base + i as u64).collect();
        assert_eq!(kernel_pass, expect);
        // and the mining kernel reports winners only from that set, verified
        let hashes = AtomicU64::new(0);
        let found = g.search(&job, Instant::now() + Duration::from_secs(5), &hashes).unwrap().expect("a nonce");
        assert!(job.verify(found));
        assert_eq!(g.false_positives, 0);
    }
}
