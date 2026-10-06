# tn10-miner

Standalone Kaspa testnet-10 miner (funds test addresses, e.g. the soak's bank): CPU threads or an OpenCL GPU, continuously or in
bursts below a balance watermark. Not part of any cargo workspace (empty `[workspace]` in Cargo.toml). Built against rusty-kaspa
`v2.1.0` (git tag).

Only `header.nonce` is changed; the template block is submitted as the raw JSON the node returned, so tx v1 / compute budget /
covenant fields are preserved. Every nonce a backend reports is verified with `kaspa_pow` on the CPU before `submitBlock`.

## Build

    cargo build --release --target-dir <somewhere-outside-repo>                   # CPU only
    cargo build --release --features gpu --target-dir <somewhere-outside-repo>    # + OpenCL backend

Binary: `<target-dir>/release/tn10-miner(.exe)`. The `gpu` feature uses the `opencl3` crate with run-time loading of
`OpenCL.dll` / `libOpenCL.so`: the build needs no OpenCL SDK, and a GPU build still runs where there is no OpenCL driver (`--backend
auto` then mines on the CPU; `--backend gpu` fails).

## Run

    WALLET=kaspatest:qq...[,kaspatest:qp...] MAX_BLOCKS=100 tn10-miner
    tn10-miner --backend gpu kaspatest:qq...    # addresses may also be CLI args
    tn10-miner balance kaspatest:qq...          # total, spendable (coinbase-matured) and immature KAS per address
    tn10-miner bench cpu|gpu|gpu-sweep [SECS]   # raw hashrate on a synthetic header, no node needed
    tn10-miner gpu-list                         # OpenCL GPU devices (index for GPU_DEVICE)

### Settings

Each setting comes from (first wins) the CLI flag (`--backend` only), the environment, the **settings file**, the default. The
settings file is JSON: `MINER_CONFIG=<path>`, else `tn10-miner.json` next to the executable if it exists. The miner **re-reads it when
it changes**: watermarks, duty cycle, poll, threads and round time apply at once; backend and GPU options on the next start. An
invalid edit is logged and ignored (the running settings stay). Unknown keys are refused.

| file key | env | default | |
|---|---|---|---|
| `backend` | `MINER_BACKEND` | `auto` (gpu build) / `cpu` | `cpu`, `gpu` or `auto` (GPU if one opens, else CPU) |
| `threads` | `THREADS` | physical cores - 1 | CPU backend threads |
| `roundMs` | `ROUND_MS` | 3000 | template refresh interval |
| `lowKas`, `highKas` | `MINER_LOW_KAS`, `MINER_HIGH_KAS` | none: continuous | intermittent mining watermarks (both or neither) |
| `maxDuty` | `MINER_MAX_DUTY` | 1 | at most this fraction of time mining ... |
| `dutyWindowSec` | `MINER_DUTY_WINDOW_SEC` | 600 | ... over this sliding window |
| `pollSec` | `MINER_POLL_SEC` | 15 | balance poll interval |
| `maturityDaa` | `MINER_MATURITY_DAA` | 1000 | coinbase maturity (TN10: 1000 DAA, ~100 s) |
| `balanceAddress` | `MINER_BALANCE_ADDRESS` | the pay address(es) | address(es) the watermarks watch |
| `gpu.device` | `GPU_DEVICE` | first GPU | index (`gpu-list`) or name substring |
| `gpu.global` | `GPU_GLOBAL` | auto-tuned | nonces per dispatch |
| `gpu.local` | `GPU_LOCAL` | 128 | work-group size |
| `gpu.dispatchMs` | `GPU_DISPATCH_MS` | 60 | auto-tune target per dispatch |

Other env: `NODE` (wRPC JSON url; falls back to `KOB_TN10_WRPC`, then `ws://127.0.0.1:18210`), `WALLET` (pay addresses, comma
separated, round-robin one per block), `MAX_BLOCKS` (stop after N accepted blocks, default 0 = unlimited).

### Intermittent mining

With `lowKas` / `highKas` the miner reads the spendable balance of the watched address(es) every `pollSec` from the node's UTXO index
(`getUtxosByAddresses` + the virtual DAA score of `getBlockDagInfo`): non-coinbase UTXOs plus coinbase UTXOs at least `maturityDaa`
old; covenant-bound UTXOs do not count. A **burst** starts when the spendable balance falls below `lowKas` and ends when it reaches
`highKas`; in between the current state holds (hysteresis). While idle the miner does nothing but poll (no template requests, no
CPU threads, no GPU work). Mined coinbase becomes spendable only after the maturity (~100 s on TN10), so a burst overshoots `highKas`
by about 100 s of mining.

`maxDuty` caps the fraction of time spent mining over the last `dutyWindowSec` (also without watermarks); a capped miner resumes when at
least min(5 s, a quarter of the allowed time per window) of budget is back.

Log lines (stderr): `mining: <reason> spendable=.. immature=.. duty=..%` / `idle: <reason> ...` on every state change,
`<rate> kH/s, accepted=.. backend=.. spendable=.. duty=..%` every 30 s while mining (rate over mining time), `idle spendable=..`
every 2 min while idle; accepted blocks on stdout.

## GPU backend

`kernels/kheavyhash.cl` (OpenCL C 1.2) is the founder's earlier TN12 GPU-miner kernel (it mined verified TN12 blocks on an Adreno 750),
kept with small changes listed in its header (shared per-nonce function, atomic result slots, a test kernel). The host computes
everything consensus-specific with rusty-kaspa: the pre-PoW hash (`hash_override_nonce_time`), the matrix (`Matrix::generate`, read
from `kaspa_pow`), the target (`Uint256::from_compact_target_bits`) and the cSHAKE256 initial states (copies of the private
`PowHash` / `KHeavyHash` constants, proven by the tests). The kernel's arithmetic was diffed against `kaspa_pow::State::calculate_pow`
step by step (see the kernel header); no divergence. Dispatch sizes auto-tune to `gpu.dispatchMs`; the host thread sleeps through the
expected kernel time and polls the event instead of a blocking wait (NVIDIA's OpenCL spins a CPU core in blocking waits).

## Tests

    cargo test                       # CPU path: host data vs kaspa_pow, throttle, settings, mock-node loop (CI)
    cargo test --features gpu        # + kernel vs kaspa_pow on fixed headers (skipped without an OpenCL GPU;
                                     #   TN10_MINER_REQUIRE_GPU=1 makes a missing GPU a failure)

`tests/mock_node.rs` runs the real loop against a mock wRPC node: template -> found -> verify -> submit (the mock re-checks every
block with `kaspa_pow`), bursts following the mock's UTXO set (immature coinbase ignored), idle above the watermark, and the duty cap.

## Measured

16-thread box, 7 threads (CPU): ~1.3-3.3 MH/s; tn10 difficulty is tiny: ~1.5-1.8 s per accepted block, ~2-3 KAS per block (220 blocks
in 405 s = 615 KAS).
