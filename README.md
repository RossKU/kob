# KOB: Kaspa Order Book

KOB is a permissionless spot limit order book for KCC-20 tokens (and, through adapters, KRON
tokens) on Kaspa layer 1. The orders are covenants written in
[SilverScript](https://github.com/kaspanet/silverscript) and shipped as an
[Argent](https://github.com/argent-lang/argent) app; there is no bridge, no sequencer and no
off-chain custody.

* **Everyone posts limit orders.** An order is one covenant UTXO holding the tokens (sell) or the
  KAS (buy). The covenant enforces price, quantity, custody, time in force and triggers on chain;
  only the maker can cancel.
* **Matchers are permissionless.** Anyone can build a batch transaction that fills crossing
  orders, and anyone can refund an expired one. A matcher earns the crossing spread plus any tips
  the orders offer. There is no protocol fee, no operator key and no admin function in the order
  contracts.
* **Order types.** Limit (GTC / GTD / day / timed), IOC, FOK, market (bounded IOC auction), stop,
  stop-limit, stop-market, trailing stop, take-profit, OCO, IFD / IFO (with repeat), TWAP / DCA /
  Dutch, cancel-replace, partial fills everywhere, and a token-to-token cross limit that settles
  through two KAS books in one transaction. See `docs/spec/order-types.md`.
* **Payments.** A [x402](https://github.com/elldeeone/kaspa-x402) profile for KCC-20 payments and
  a swap-and-pay router (pay a merchant in token B from token A or KAS) built as an Argent app.

## Status

**Pre-audit. Testnet-10 only. Not for mainnet funds.**

* No independent audit has been done. The author's own review passes and an extensive test and
  mutation suite (`tools/ablation`) exist; they are not a substitute. See `SECURITY.md`.
* Nothing is deployed to mainnet. The only network the project runs on is testnet-10.
* Token registry (`registry/tokens.json`): the two KRON token programs are `reviewed`, with conditions
  (each token still needs its genesis check and a no-live-minter scan before it is listed, and every
  token entry is `pending-review`); the KCC-20 programs, KaspaCom's included, are `pending-review`.
  The KCC-20 token standard is in Last Call, not final; KOB's token programs follow its merged reference program, and KOB
  issues that program in its standard 3 / 3 configuration (the other slot variants are prototypes;
  `docs/spec/kcc-conformance.md`).
* The Argent app build needs two patches to `argentc` that are not upstream (`argent/UPSTREAM.md`).
  Open questions for the Argent team are in `docs/argent-feedback.md`.
* Interfaces, encodings and artifact ids can still change.

## Repository map

| Path | Contents |
|---|---|
| `contracts/` | SilverScript sources (`v2/` protocol v2 orders, `orders/` first-generation orders, `kcc20/`, `adapters/kron/`), compiled `artifacts/` (committed, reproducible), `silverc.lock`, `SHA256SUMS`, `deploy/` (per-network deployment records), and the Argent app in `contracts/argent/` |
| `contracts/third-party/` | Pinned third-party token programs: KaspaCom KCC20 0.2.5 (Apache-2.0), and the KCC-20 reference source and its published public-mint build (argent-lang/kcc20-reference `c8a0871`, ISC) |
| `crates/kob-protocol` | Protocol library: pinned artifacts, state and sigscript encoding, builders for every order action and matcher shape, fee and mass pass, compute-budget table, `KOB1` payload, engine validation, golden vectors |
| `crates/kob-wasm` | wasm-bindgen bindings over `kob-protocol` (used by the web UI and the TypeScript SDK) |
| `crates/kob-executor` | One binary: indexer + read API (REST and WebSocket), matcher, keepers, and the x402 facilitator |
| `crates/kob-x402` | x402 wire types, verifiers (KAS, KCC-20, swap-and-pay), payer and merchant SDK, in-memory chain test kit |
| `crates/kob-cli` | Command line: `kob token issue`, `kob registry validate` (the rest is planned) |
| `crates/kob-tests` | The contracts executed in the rusty-kaspa v2.1.0 script engine: positive and negative harnesses |
| `web/` | Static, client-side, non-custodial web UI (TypeScript, Preact) over `kob-wasm`; mock indexer and node for offline use |
| `packages/kob-x402` | TypeScript x402 SDK and an example paid API |
| `registry/` | Token registry: strict template list and open token list |
| `argent/` | Pinned Argent upstream commit, our two patches, reproducible `argentc` build |
| `vendor/` | SilverScript v1.0.0 (patched for rusty-kaspa v2.1.0) and the Argent artifact crate |
| `tools/` | `ablation` (contract mutation runner), `sil2argent` (publishes hand-written contracts as an Argent artifact), `router-gen` (router generator), `soak` (testnet market simulator), `wallet-gate` (browser-wallet covenant-signing checks) |
| `scripts/` | `build-contracts.sh`, `build-argent.sh`, `build-wasm.sh`, `build-deploy.sh` (and `lib/`, their shared helpers) |
| `docs/` | Specifications and operator guides, see [docs/README.md](docs/README.md) |

## Quickstart

Requirements: `rustup` (the toolchain, Rust 1.94.0 with the wasm32 target, is pinned by
`rust-toolchain.toml`), a POSIX shell (Git Bash on Windows) for the scripts, and Node 22 or
newer for the web UI and tools. rusty-kaspa is a git dependency at tag `v2.1.0`, so the first
build downloads it.

**Build the contracts.** The committed artifacts must reproduce byte for byte:

```
scripts/build-contracts.sh --check                # silverc from vendor/silverscript, argentc from the pinned upstream
scripts/build-contracts.sh --check --no-argent    # only the SilverScript artifacts
scripts/build-contracts.sh                        # rewrite the artifacts after editing a .sil
```

**Run the tests.**

```
KOB_SKIP_NETWORK_TESTS=1 cargo test --workspace --locked
KOB_CARRIER_KAS=20 cargo test --locked -p kob-tests          # harness order carrier of 2 (the wallet default), 10 (default) or 20 KAS
scripts/build-wasm.sh --install-bindgen --test               # wasm bindings + node golden-vector tests
node tools/ablation/ablate.mjs --list --suite all            # contract mutation catalogue (see tools/ablation/README.md)
```

Without `KOB_SKIP_NETWORK_TESTS=1` some `kob-executor` tests contact a testnet-10 node and mine
their own funds. Do that only on purpose.

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
your own settings and a browser wallet; see `web/README.md`. Do not use it with mainnet funds
yet.

**Run `kob-executor` against testnet-10.** You need a testnet-10 node with the JSON wRPC
listener (`kaspad --utxoindex --testnet --netsuffix=10 --rpclisten-json=127.0.0.1:18210`,
see `docs/ops/executor.md`, Part B). The testnet-10 build (`deploy-tn10`) records the network of
`contracts/deploy/testnet-10` and refuses any other `--network`; its templates are the reference ones
(no template depends on a network, and there is no genesis step):

```
cargo build --release --locked -p kob-executor --features deploy-tn10
target/release/kob-executor index --network testnet-10 --rpc-url ws://127.0.0.1:18210 \
    --data-dir ./kob-data --tokens registry/tokens.example.json --listen 127.0.0.1:8090
```

The mainnet release build is `--features deploy-mainnet` (it pins `registry/tokens.json`); the release checklist, the
hashes to publish, the upgrade order and the operator requirements are in `docs/ops/release.md`.

`registry/tokens.example.json` lists fictional testnet tokens: replace it with your own allowlist (`registry/README.md`). `index` is the keyless, read-only role. `kob-executor run` adds the matcher and keepers and needs
an operator hot key (`KOB_OPERATOR_KEY_FILE`, test funds only); `docs/ops/executor.md` covers
flags, keys, monitoring and the runbook.

## Documentation

See [docs/README.md](docs/README.md) for the index. The main entries:

* `docs/spec/matcher.md`: normative matcher, executor and wallet rules
* `docs/spec/order-types.md`: the order types and their semantics
* `docs/spec/kob1-payload.md`: the `KOB1` payload and placement record
* `docs/spec/x402-kcc20-profile.md`, `docs/spec/x402-swap-and-pay.md`: x402 proposals; `docs/spec/x402-retry.md`: how a
  payment is retried without paying twice
* `docs/argent.md`: KOB as an Argent app, how to import it, the router
* `docs/ops/executor.md`: running the indexer, matcher and keepers
* `contracts/README.md`, `registry/README.md`: contracts and token registry

## Security and disclaimer

This is experimental software that handles funds. It is unaudited, may contain bugs that lose
money, and is provided as is, without warranty. Report vulnerabilities privately as described in
[SECURITY.md](SECURITY.md). Running a matcher or a facilitator is up to whoever chooses to do
it; the project operates no production service.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## Licence

ISC, see [LICENSE](LICENSE). Third-party code keeps its own licence: see [NOTICE](NOTICE) for
the list (SilverScript, Argent artifact crate, KaspaCom KCC20, test vectors, charting library).
