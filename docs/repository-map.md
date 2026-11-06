# Repository map

| Path | Contents |
|---|---|
| `contracts/` | What is deployed and what to audit: [contracts/README.md](../contracts/README.md). `orders/` the live order covenants, `orders/kron/` their KRON twins, `kcc20/` the standalone reference token, `prototypes/` and `research/` (not deployed), `adapters/kron/templates/` (KRON token programs), compiled `artifacts/` (committed, reproducible), `silverc.lock`, `SHA256SUMS`, `deploy/` (per-network deployment records), and the Argent app in `argent/` |
| `contracts/third-party/` | Pinned third-party token programs, each with its provenance (`UPSTREAM.md`): KaspaCom KCC20 0.2.5 (Apache-2.0), the KCC-20 reference source and its published public-mint build (argent-lang/kcc20-reference `c8a0871`, ISC), and programs observed on chain (`kasparocket/`, `kforge/`, `kforge-kcc20/`, `kusdt/`) |
| `crates/kob-protocol` | Protocol library: pinned artifacts, state and sigscript encoding, builders for every order action and matcher shape, fee and mass pass, compute-budget table, `KOB1` payload, engine validation, golden vectors |
| `crates/kob-wasm` | wasm-bindgen bindings over `kob-protocol` (used by the web UI and the TypeScript SDK) |
| `crates/kob-executor` | The `kobd` daemon: the Indexer (full history, record log, read API over REST and WebSocket) and the Executor (matcher, keepers, x402 facilitator), alone or together; its `Dockerfile` builds a `kobd` image of one network from source (`docs/ops/executor-quickstart.md`, section 6) |
| `crates/kob-x402` | x402 wire types, verifiers (KAS, KCC-20, swap-and-pay), payer and merchant SDK, in-memory chain test kit |
| `crates/kob-cli` | Command line ([README](../crates/kob-cli/README.md)): trading (`kob place` with every order kind of the web ticket, `replace`, `cancel`, `refund`, `take`, `send`, `merge`, planned like the web wallet; `book`, `orders`, `balances`), `kob token create`, `kob registry` (validate, verify-genesis, programs, add-program), `kob recover`, `kob order sweep` / `cancel` |
| `crates/kob-tests` | The contracts executed in the rusty-kaspa v2.1.0 script engine: positive and negative harnesses |
| `web/` | Static, client-side, non-custodial web UI (TypeScript, Preact) over `kob-wasm`; mock indexer and node for offline use; module map in [web/src/README.md](../web/src/README.md) |
| `packages/kob-x402` | TypeScript x402 SDK and an example paid API |
| `registry/` | Token registry: the supported programs and a pinned snapshot of token facts (no curation) |
| `vendor/` | SilverScript v1.0.0 (patched for rusty-kaspa v2.1.0), the Argent artifact crate, and `argent-upstream/`: the pinned Argent commit, our two patches, the reproducible `argentc` build |
| `tools/` | `ablation`, `sil2argent`, `router-gen`, `soak`, `wallet-gate`: role of each and whether CI uses it in [tools/README.md](../tools/README.md) |
| `android/` | The testnet-10 Android app: `kobd` in a foreground service with a setup and status screen (`docs/ops/executor-quickstart.md`, section 9) |
| `scripts/` | Builds (`build-contracts.sh`, `build-argent.sh`, `build-wasm.sh`, `build-deploy.sh`, `build-state-dicts.mjs`; `lib/`, their shared helpers), checks (`check-pins.mjs`, `check-wallet-gate.mjs`) and release tooling (`release-hashes.sh`, `package-release.mjs`, `make-android-keystore.mjs`; `docs/ops/release.md`) |
| `docs/` | Specifications and operator guides, see [README.md](README.md) |
