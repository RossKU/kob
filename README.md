# KOB: Kaspa Order Book

KOB is a permissionless spot limit order book for KCC-20 tokens (and, through adapters, KRON
tokens) on Kaspa layer 1. The orders are covenants written in
[SilverScript](https://github.com/kaspanet/silverscript) and shipped as an
[Argent](https://github.com/argent-lang/argent) app; there is no bridge, no sequencer and no
off-chain custody.

* **Everyone posts limit orders.** An order is one covenant UTXO holding the tokens (sell) or the
  KAS (buy): an amount of the base token at a price in the quote token per whole base token,
  rounded in the maker's favour. The covenant enforces price, quantity, custody, time in force and
  triggers on chain; only the maker can cancel.
* **Matchers are permissionless.** Anyone can build a batch transaction that fills crossing
  orders, and anyone can refund an expired one. A matcher earns the crossing spread plus any tips
  the orders offer. There is no protocol fee, no operator key and no admin function in the order
  contracts.
* **Order types.** Limit (GTC / GTD / day / timed), IOC, FOK, market (bounded IOC auction), stop,
  stop-limit, stop-market, trailing stop, take-profit, OCO, IFD / IFO (with repeat), TWAP / DCA /
  Dutch, cancel-replace, partial fills everywhere. Every type also exists on a token/token pair: a pair order
  is netted against opposite pair orders or settled through the two KAS books in one transaction; prices,
  charts and triggers come from the KAS books. See `docs/spec/order-types.md`.
* **Payments.** A [x402](https://github.com/elldeeone/kaspa-x402) profile for KCC-20 payments and
  a swap-and-pay router (pay a merchant in token B from token A or KAS) built as an Argent app.

## Start here

* **Trade.** Open the web app, connect KasWare or Kaspire (a desktop extension) and place an order: [docs/user-guide.md](docs/user-guide.md)
  (connect, buy and sell, limit and market orders, partial fills, cancel, merge, the status banners, fees, the private-key option,
  recovering a token you created). Testnet-10 coins have no value; use them first.
* **Run an Indexer** (serve the order book, no key) **or an Executor** (match orders and earn the spread and tips): build `kobd`
  from source and start it, [below](#run-kobd-on-testnet-10-from-source); the step-by-step operator page (services, Docker,
  Android, upgrades, withdrawals) is [docs/ops/executor-quickstart.md](docs/ops/executor-quickstart.md).
* **Host the web app and a public read server:** [docs/ops/web-hosting.md](docs/ops/web-hosting.md) and
  [docs/ops/chart-server.md](docs/ops/chart-server.md).
* **Build a bot or an integration:** the read API is documented in [docs/api.md](docs/api.md); orders are placed with the command
  line [crates/kob-cli/README.md](crates/kob-cli/README.md) (`--json`) or the web app.
* **Develop:** [Develop and test](#develop-and-test) below and [CONTRIBUTING.md](CONTRIBUTING.md).

## Status

**Pre-audit. Mainnet launches as a canary: use at your own risk, only funds you can afford to lose.**

* No external audit is planned before launch. An internal audit (`docs/security/AUDIT-2026-10.md`) and test,
  mutation (`tools/ablation`) and fuzz suites exist; they are not a substitute. See `SECURITY.md`.
* Testnet-10 remains the network for testing.
* Tokens of the programs this build supports (the KCC-20 3 / 3 reference program, its public-mint build, the two KRON
  programs) are tradable with no listing step: KOB makes no per-token listing decision, and a token whose genesis the
  registry does not record as checked carries the `genesis_unverified` warning; other programs are `pending-review`
  (`registry/README.md`, `docs/spec/token-programs.md`).
* KCC-20 is in Last Call, not final (`docs/spec/kcc-conformance.md`).
* The Argent build needs two `argentc` patches that are not upstream (`argent/UPSTREAM.md`).
* Interfaces, encodings and artifact ids can still change.

## Run `kobd` on testnet-10 (from source)

`kobd` is one program with two roles. The **Indexer** (`kobd index`) is keyless and read-only: it follows the chain, keeps the
order history and serves the read API the web app and bots use. The **Executor** (`kobd run`) adds the matcher and the keepers:
it fills crossing orders and refunds expired ones with a funded **hot key** and earns the spreads and tips. Building from source
is the supported route; tagged releases (`docs/ops/executor-quickstart.md`, section 1) can lag `master`. Testnet-10 coins have no
value: start there.

**1. Prerequisites.** Git, [rustup](https://rustup.rs) (the pinned toolchain, Rust 1.94.0, installs itself on the first
build) and a C compiler (two dependencies, SQLite and secp256k1, compile C code). About 6 GB of disk for the build, a few GB
for an Indexer's data; no OpenSSL, protoc or clang.

* **Windows 10 / 11:** [Git for Windows](https://git-scm.com/download/win), rustup with its default MSVC toolchain, and the
  Visual Studio Build Tools with *Desktop development with C++* (the rustup installer offers them). Use PowerShell.
* **Linux:** `sudo apt install git curl build-essential` (or your distribution's equivalent), then rustup.

**2. Build.** The `deploy-tn10` build serves testnet-10 only and refuses any other network. The first build downloads
rusty-kaspa `v2.1.0` and takes some minutes.

```
git clone https://github.com/RossKU/kob.git
cd kob
cargo build --release --locked -p kob-executor --features deploy-tn10
```

The binary is `target/release/kobd` (`target\release\kobd.exe` on Windows); `kobd --version` prints `kobd 0.1.0`. Give it a
folder of its own, outside the repository and outside any folder a cloud service syncs: its configuration, key and data live
next to it.

```
# Linux                                         # Windows (PowerShell)
mkdir ~/kob-tn10                                mkdir C:\kob-tn10
cp target/release/kobd ~/kob-tn10/              copy target\release\kobd.exe C:\kob-tn10\
cd ~/kob-tn10                                   cd C:\kob-tn10
```

**3. An Indexer (no key, no funds).**

```
./kobd setup --non-interactive --node resolver --indexer     # writes kob-executor.toml and kob-tokens.json
./kobd                                                         # runs it in the foreground: kobd index --config kob-executor.toml
curl -s http://127.0.0.1:8090/v1/health                        # in a second terminal (Windows: curl.exe -s ...)
```

(On Windows write `.\kobd.exe` for `./kobd`.) `./kobd` with no configuration in its folder runs the same setup as a dialogue
instead (Enter takes each default). The health answer names `"network":"testnet-10"`, `"roles":["indexer"]` and a `state`
that goes `starting`, `catching_up`, `following`: the first start reads everything the node retains (about 36 hours of
chain, roughly 2.5 hours over public nodes) and `ok` turns `true` once the lag alarms clear. For the fast route (the books
from now on only), put `start = "sink"` at the top of `kob-executor.toml` (above `[launch]`) before the first start. A
public read server behind a tunnel: `docs/ops/chart-server.md`.

**4. An Executor (a funded hot key).** Give it a second folder with its own copy of `kobd` (for example `~/kob-tn10-exec`;
two processes cannot share a data directory) and run, in that folder:

```
./kobd setup --non-interactive --node resolver --executor --new-key     # prints the key's funding "address"
./kobd run --config kob-executor.toml --key-file kob-operator.key --pause-file kob-data/pause --dry-run
./kobd                                                                     # live: the same command without --dry-run
```

The setup generates `kob-operator.key` (64 hex characters, readable by your account only) and turns on lean storage and the
fast start. Send the address at least 100 testnet KAS (mine them or ask a faucet), as a few transfers of 10 KAS or more;
`./kobd address-balance` shows the balance. `--dry-run` builds and validates every transaction and submits nothing: let it run
until its status line says `indexer following` and `/v1/health` says `"roles":["executor"]`, stop it, then start `./kobd`. The
key is a hot key and KOB is pre-audit: keep only working capital on it, never more than you can afford to lose, and back the
file up where only you can read it.

**What runs where.**

| What | Where |
|---|---|
| Node | `rpc_url = "resolver"`: two public testnet-10 nodes found through the Kaspa resolvers (outbound HTTPS and WSS, port 443), one cross-checking the other. Your own node instead: `kaspad --testnet --netsuffix=10 --utxoindex --rpclisten-json=default` and `--node ws://127.0.0.1:18210` in the setup (required for Miner Mode; `docs/ops/executor-quickstart.md`, section 2) |
| Ports | the read API on `127.0.0.1:8090` (loopback; `--listen <addr>` or `[api] listen`). Nothing listens publicly and no inbound port is needed |
| Health | `GET /v1/health` (`ok`, `state`, `roles`, `network`, lag), `GET /v1/health/ready` (200 / 503 for a probe); fields in `docs/api.md` |
| Files | `kob-executor.toml` (the configuration; `./kobd` starts it in its own folder, so its relative paths are taken from there: run the other commands from that folder too), `kob-tokens.json` (the supported token programs), `kob-operator.key` (Executor only), `kob-data/` (the SQLite store and, for the Indexer, the record log) |
| Logs | the console, with a status line every minute; `--log-file kob-executor.log` (or `log_file = "kob-executor.log"` in the configuration) also writes a rotated file. `RUST_LOG=debug` for more |
| Pause, stop | the file `kob-data/pause` pauses an Executor's trading; Ctrl+C stops either role cleanly; the next start continues where it stopped |

Next: services (Task Scheduler, systemd), Docker, upgrades and withdrawals in `docs/ops/executor-quickstart.md`; every flag,
the configuration keys, the economics and the runbook in `docs/ops/executor.md`. `kobd --help` and `kobd <command> --help` list
the options. The mainnet build is `--features deploy-mainnet`; mainnet is a pre-audit canary (`docs/ops/release.md`).

## Develop and test

Requirements: the prerequisites of the section above, the `wasm32-unknown-unknown` target (`rust-toolchain.toml` adds it),
a POSIX shell (Git Bash on Windows) for the scripts, and Node 22 or newer for the web UI and tools.

**Build the contracts.** The committed artifacts must reproduce byte for byte:

```
scripts/build-contracts.sh --check                # silverc from vendor/silverscript, argentc from the pinned upstream
scripts/build-contracts.sh --check --no-argent    # only the SilverScript artifacts
scripts/build-contracts.sh                        # rewrite the artifacts after editing a .sil
```

**Run the tests.**

```
KOB_SKIP_NETWORK_TESTS=1 cargo test --workspace --locked
KOB_CARRIER_KAS=20 cargo test --locked -p kob-tests          # harness order carrier of 1 (the wallet default), 2, 10 (default) or 20 KAS
scripts/build-wasm.sh --install-bindgen --test               # wasm bindings + node golden-vector tests
node tools/ablation/ablate.mjs --list --suite all            # contract mutation catalogue (see tools/ablation/README.md)
```

Without `KOB_SKIP_NETWORK_TESTS=1` some `kob-executor` tests contact a testnet-10 node and mine
their own funds. Do that only on purpose. `CONTRIBUTING.md` lists the lint and the other checks CI runs.

**Run the web UI in mock mode** (no node, no wallet, no network):

```
scripts/build-wasm.sh --install-bindgen --web --slim
cd web
npm ci
npm run fetch-sdk                          # the official rusty-kaspa v2.1.0 wasm SDK, not the npm package
npm run build:wasm -- --copy
npm run mock -- --history --fund           # mock indexer + node on http://127.0.0.1:8790
npm run dev                                # then Settings: indexer URL and node URL = http://127.0.0.1:8790
```

The web app defaults to mainnet, no indexer and the SDK's public resolver. A real run needs
your own settings and a browser wallet; see `web/README.md`. On mainnet it is a pre-audit canary:
use only funds you can afford to lose.

**Builds of `kobd`.** `--features deploy-tn10` (above) records the network of `contracts/deploy/testnet-10` and refuses any
other; its templates are the reference ones (no template depends on a network, and there is no genesis step). The reference
build (no feature) needs `--network` on every command; `--features deploy-mainnet` pins `registry/tokens.json`, and its release
checklist, the hashes to publish, the upgrade order and the operator requirements are in `docs/ops/release.md`. A token list of
your own goes in `tokens_path` (`registry/tokens.example.json` is a format example with fictional testnet tokens;
`registry/README.md`).

## Repository map

| Path | Contents |
|---|---|
| `contracts/` | SilverScript sources (`v2/` the order covenants, `orders/` the original plain orders of the v1 test suites, `kcc20/`, `adapters/kron/`), compiled `artifacts/` (committed, reproducible), `silverc.lock`, `SHA256SUMS`, `deploy/` (per-network deployment records), and the Argent app in `contracts/argent/` |
| `contracts/third-party/` | Pinned third-party token programs, each with its provenance (`UPSTREAM.md`): KaspaCom KCC20 0.2.5 (Apache-2.0), the KCC-20 reference source and its published public-mint build (argent-lang/kcc20-reference `c8a0871`, ISC), and programs observed on chain (`kasparocket/`, `kforge/`, `kforge-kcc20/`, `kusdt/`) |
| `crates/kob-protocol` | Protocol library: pinned artifacts, state and sigscript encoding, builders for every order action and matcher shape, fee and mass pass, compute-budget table, `KOB1` payload, engine validation, golden vectors |
| `crates/kob-wasm` | wasm-bindgen bindings over `kob-protocol` (used by the web UI and the TypeScript SDK) |
| `crates/kob-executor` | The `kobd` daemon: the Indexer (full history, record log, read API over REST and WebSocket) and the Executor (matcher, keepers, x402 facilitator), alone or together |
| `crates/kob-x402` | x402 wire types, verifiers (KAS, KCC-20, swap-and-pay), payer and merchant SDK, in-memory chain test kit |
| `crates/kob-cli` | Command line ([README](crates/kob-cli/README.md)): trading (`kob place` with every order kind of the web ticket, `replace`, `cancel`, `refund`, `take`, `send`, `merge`, planned like the web wallet; `book`, `orders`, `balances`), `kob token create`, `kob registry` (validate, verify-genesis, programs, add-program), `kob recover`, `kob order sweep` / `cancel` |
| `crates/kob-tests` | The contracts executed in the rusty-kaspa v2.1.0 script engine: positive and negative harnesses |
| `web/` | Static, client-side, non-custodial web UI (TypeScript, Preact) over `kob-wasm`; mock indexer and node for offline use |
| `packages/kob-x402` | TypeScript x402 SDK and an example paid API |
| `registry/` | Token registry: the supported programs and a pinned snapshot of token facts (no curation) |
| `argent/` | Pinned Argent upstream commit, our two patches, reproducible `argentc` build |
| `vendor/` | SilverScript v1.0.0 (patched for rusty-kaspa v2.1.0) and the Argent artifact crate |
| `tools/` | `ablation` (contract mutation runner), `sil2argent` (publishes hand-written contracts as an Argent artifact), `router-gen` (router generator), `soak` (testnet market simulator), `wallet-gate` (browser-wallet covenant-signing checks) |
| `android/` | The testnet-10 Android app: `kobd` in a foreground service with a setup and status screen (`docs/ops/executor-quickstart.md`, section 9) |
| `scripts/` | Builds (`build-contracts.sh`, `build-argent.sh`, `build-wasm.sh`, `build-deploy.sh`, `build-state-dicts.mjs`; `lib/`, their shared helpers), checks (`check-pins.mjs`, `check-wallet-gate.mjs`) and release tooling (`release-hashes.sh`, `package-release.mjs`, `make-android-keystore.mjs`; `docs/ops/release.md`) |
| `Dockerfile` | A `kobd` image of one network, built from source (`docs/ops/executor-quickstart.md`, section 6) |
| `docs/` | Specifications and operator guides, see [docs/README.md](docs/README.md) |

## Documentation

[docs/README.md](docs/README.md) is the index of every document. The ones most readers need:

* `docs/user-guide.md`: trading in the web app (wallets, orders, fees, recovery); `docs/faq.md`: who signs what, front-running,
  settlement, stablecoin pricing on a KAS book
* `docs/ops/executor-quickstart.md`: running an Indexer or an Executor step by step; `docs/ops/executor.md`: the full operations
  guide (every flag and configuration key, storage, the runbook, the matcher's economics)
* `docs/api.md`: the read API (HTTP and WebSocket); `crates/kob-cli/README.md`: the `kob` command line
* `docs/spec/`: the normative rules (`matcher.md`, `order-types.md`, `kob1-payload.md`, the x402 profiles, token programs)
* `docs/security/AUDIT-2026-10.md`: the internal pre-mainnet security audit

## Security and disclaimer

This is experimental software that handles funds. It has had no external audit, may contain bugs that lose
money, and is provided as is, without warranty. Report vulnerabilities privately as described in
[SECURITY.md](SECURITY.md). Running a matcher or a facilitator is up to whoever chooses to do
it; the project operates no production service.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## Licence

ISC, see [LICENSE](LICENSE). Third-party code keeps its own licence: see [NOTICE](NOTICE) for
the list (SilverScript, Argent artifact crate, KaspaCom KCC20, test vectors, charting library).
