# Repository map

| Path | Contents |
|---|---|
| `contracts/` | What is deployed and what to audit, see [contracts/README.md](../contracts/README.md). It holds `orders/` (the live order covenants), `orders/kron/` (their KRON twins), `kcc20/` (the standalone reference token), `prototypes/` and `research/` (not deployed), `adapters/kron/templates/` (KRON token programs), compiled `artifacts/` (committed, reproducible), `silverc.lock`, `SHA256SUMS`, `deploy/` (per-network deployment records) and the Argent app in `argent/` |
| `contracts/third-party/` | Pinned third-party token programs, each with its provenance in `UPSTREAM.md`. They are KaspaCom KCC20 0.2.5 (Apache-2.0), the KCC-20 reference source with its published public-mint build (argent-lang/kcc20-reference `c8a0871`, ISC), and programs observed on chain (`kasparocket/`, `kforge/`, `kforge-kcc20/`, `kusdt/`) |
| `crates/kob-protocol` | Protocol library: pinned artifacts, state and sigscript encoding, builders for every order action and matcher shape, fee and mass pass, compute-budget table, `KOB1` payload, engine validation, golden vectors |
| `crates/kob-wasm` | wasm-bindgen bindings over `kob-protocol`, used by the web UI and the TypeScript SDK |
| `crates/kobd` | The `kobd` daemon. It runs the Indexer (full history, record log, read API over REST and WebSocket), the Executor (matcher, keepers, x402 facilitator), or both. Its `Dockerfile` builds a `kobd` image of one network from source, see `docs/ops/executor-quickstart.md`, section 6 |
| `crates/kob-x402` | x402 wire types, verifiers (KAS, KCC-20, swap-and-pay), payer and merchant SDK, in-memory chain test kit |
| `crates/kob-cli` | Command line, see [README](../crates/kob-cli/README.md). It trades (`kob place` with every order kind of the web ticket, `replace`, `cancel`, `refund`, `take`, `send`, `merge`, planned like the web wallet, and `book`, `orders`, `balances`). It also has `kob token create`, `kob registry` (validate, verify-genesis, programs, add-program), `kob recover` and `kob order sweep` / `cancel` |
| `crates/kob-tests` | The contracts executed in the rusty-kaspa v2.1.0 script engine. Positive and negative harnesses, by area in `tests/` (`kob/`, `kcc20/`, `argent/`, `audit/`, `programs/`) |
| `web/` | Static, client-side, non-custodial web UI (TypeScript, Preact) over `kob-wasm`, with a mock indexer and node for offline use. The module map is in [web/src/README.md](../web/src/README.md) |
| `packages/kob-x402` | TypeScript x402 SDK and an example paid API |
| `registry/` | Token registry: the supported programs and a pinned snapshot of token facts, with no curation |
| `vendor/` | SilverScript v1.0.0 (patched for rusty-kaspa v2.1.0), the Argent artifact crate, and `argent-upstream/` (the pinned Argent commit, our two patches and the reproducible `argentc` build) |
| `tools/` | `ablation`, `sil2argent`, `router-gen`, `soak` and `wallet-gate`. [tools/README.md](../tools/README.md) gives the role of each and whether CI uses it |
| `android/` | The testnet-10 Android app: `kobd` in a foreground service with a setup and status screen, see `docs/ops/executor-quickstart.md`, section 9 |
| `scripts/` | Builds (`build-contracts.sh`, `build-argent.sh`, `build-wasm.sh`, `build-deploy.sh`, `build-state-dicts.mjs`, and their shared helpers in `lib/`), checks (`check-pins.mjs`, `check-wallet-gate.mjs`, `check-comment-only.cjs`) and release tooling (`release-hashes.sh`, `package-release.mjs`, `make-android-keystore.mjs`, see `docs/ops/release.md`) |
| `docs/` | Organised by audience, see [README.md](README.md). Guides are at the top, then `spec/`, `ops/`, `security/`, `argent/` and the dated `reports/` |
