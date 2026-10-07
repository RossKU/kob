# KOB mainnet release checklist

What a release manager does to cut a mainnet release, and what an operator needs to run it. KOB ships
software only (ISC): the release is a commit, its reproducible artifacts and the hashes below; the
mainnet services (node, indexer, matcher, keepers, x402 facilitator) are run by community operators.

Nothing in KOB has been independently audited. The order contracts and the token programs on the strict
list were reviewed internally only, and nothing on chain can pause or rescue an order. The launch
therefore runs with a strict template list (section 3).

## 1. No genesis step: the build is reproducible from source

Since protocol v2.6 a stop is armed by a fill in the same transaction (touch trigger). There is no receipt
covenant, no `R_ID` and no transaction to broadcast before the release: every order template is the
reference artifact of `contracts/artifacts`, the same on every network. A mainnet deployment is therefore a
**record**, not a rebuild:

| Piece | Source | Reproduced by |
|---|---|---|
| Order and token templates | `contracts/**/*.sil`, `contracts/silverc.lock`, `contracts/adapters/kron/templates/*.bin` | `scripts/build-contracts.sh --check` (and `--upstream` with the official silverc binary) |
| Mainnet deployment record | `contracts/deploy/mainnet/` (`deployment.json`, `DEPLOYMENT.md`, `SHA256SUMS`) | `scripts/build-deploy.sh mainnet --check` |
| Token registry (pinned) | `registry/tokens.json` (its sha256 is in the record) | the same `--check`, and `kob registry verify-genesis` for the genesis facts |
| Binaries | `cargo build --release --locked --features deploy-mainnet` | from the commit; publish the hash of what you built (section 6) |
| Web app | `web/` (`npm ci`, `npm run fetch-sdk` with its pinned SDK zip hash, `npm run build:wasm`, `npm run build:release`) | from the commit; publish the hash of every file of `web/dist` |

The record and the registry pin are checked in every build: `kob-protocol`'s
`mainnet_deployment_record_matches_the_embedded_templates_and_registry` test fails when a template or
`registry/tokens.json` changed without `scripts/build-deploy.sh mainnet`. CI runs `build-deploy.sh --check`
for testnet-10 and mainnet.

## 2. Release gates (all must hold before a mainnet release)

1. Wallet signing gate passed for the wallets the release supports (covenant inputs signed by the wallet).
2. KCC-20 template bytes pinned against the merged upstream specification and its conformance vectors
   (`crates/kob-tests/vectors`); until then the KCC-20 programs stay `pending-review` (not on the strict list).
3. Internal review: no open high or critical finding.
4. The TN10 soak completed by the operator, including the incident, `recover` and backup / restore drills
   (`docs/ops/executor.md` 7.8).
5. Legal review of the OSS-provider position.

## 3. Registry for launch

`registry/tokens.json` is the mainnet registry the release pins. At launch (founder decision 2026-10-03):

* **Strict template list:** the two KRON programs (`kron-2433`, `kron-2732`), reviewed with conditions. The
  reference KCC-20 programs and KaspaCom's KCC20 0.2.5 are `pending-review` (finding K-1), so their tokens
  are not listed.
* **Tokens:** the eight KRON tokens of the census are listed with their genesis records (genesis verified on
  mainnet data, no live mint authority). Six are `official`: KRON, KASCOV, ANSEM, IFWEN, PEEPS, KDIST.
  **PEPE** ("The Ultimate test") and **DNBT** ("dont buy this is test") are listed and verified but **not
  official**: they are test tokens per their own names, and each carries a maintainer `warning` that every UI
  shows (PEPE's also says it is not the well-known PEPE: the ticker collides). A token on another program
  that copies one of these two tickers is shown as a ticker collision, not as an impersonation
  (`lookalikeReport` level `shared`): the registry itself disowns them.
* Before the release: `cargo run -p kob-cli -- registry verify-genesis --network mainnet` must pass on the
  committed evidence (`registry/README.md`, "Genesis check").

Any registry change after the record was written makes the record stale: rerun
`scripts/build-deploy.sh mainnet` and commit the result together with the registry change.

## 4. Build

From a clean checkout of the release commit (`git status` clean; Rust toolchain from `rust-toolchain.toml`,
node 22):

```
scripts/build-contracts.sh --check
scripts/build-contracts.sh --check --upstream --no-argent
scripts/build-deploy.sh testnet-10 --check
scripts/build-deploy.sh mainnet --check
cargo run --locked -p kob-cli -- registry verify-genesis --network mainnet

# binaries: the deployment build records mainnet, refuses any other --network, and embeds the pinned registry
cargo build --release --locked -p kob-executor -p kob-cli --features deploy-mainnet

# web app (static; network-independent bundle whose default network is mainnet and whose registry pin is
# the sha256 of registry/tokens.json at build time)
cd web && npm ci && npm run fetch-sdk && npm run build:wasm && npm run typecheck && npm test && npm run build:release && cd ..

# optional: node / bot bindings of the deployment variant (crates/kob-wasm/pkg-node-mainnet, pkg-mainnet)
scripts/build-wasm.sh --mainnet --web
```

`deploy-mainnet` and `deploy-tn10` are exclusive (a compile error names both). The templates are the same in
every build; the feature only records the network and pins the registry: `kob-executor` refuses a configured
network other than `mainnet`, and without `--tokens` it lists the tokens of the embedded, pinned registry
(an operator may still pass `--tokens` with an own list; the executor logs that its hash differs from the pin,
and `GET /v1/tokens` reports `registry.source` and `registry.sha256` either way).

The web bundle carries no network-dependent code (the browser build is the slim kob-wasm variant: no x402
payer entry point); its defaults are `network: mainnet` and `./registry/tokens.json`, which `npm run build:release`
ships in `dist/registry/` (the pinned file; a plain `npm run build` leaves the registry to the host). A hosting
operator may set `config.json` next to the app (`indexerUrl`, `nodeUrl`, ...); a registry other
than the pinned one is labelled "non-default registry" and none of its tokens is shown as official.

## 5. Tests before tagging

```
cargo fmt --all --check
cargo clippy --locked --all-targets -p kob-protocol -p kob-wasm -p kob-executor -p kob-cli -p kob-tests -p sil2argent -- -D warnings
KOB_SKIP_NETWORK_TESTS=1 cargo test --workspace --locked --no-fail-fast
cargo test --locked -p kob-protocol -p kob-executor --lib --features deploy-mainnet
scripts/build-wasm.sh --test
cd web && npm test && npm run e2e
```

## 6. Hashes to publish

```
scripts/release-hashes.sh target/release/kob-executor target/release/kob-cli
```

prints the commit, every order template hash of the record, the registry sha256 and the sha256 of
`contracts/SHA256SUMS`, the deployment record, every file of `web/dist` and the binaries given. It refuses
to run when the record does not reproduce, and when the web bundle does not embed the pinned registry hash or
does not ship the pinned file as `dist/registry/tokens.json`.
Publish its output with the release (release notes, the operator announcement and an on-chain `KOB-REL`
anchor when one is made). Users and operators check:

* the template hashes against `contracts/deploy/mainnet/DEPLOYMENT.md` of the commit and the chain (an order's
  P2SH is the template over its state; kob-wasm `templates()` lists what a build embeds);
* the registry hash against `GET /v1/tokens` `registry.sha256` of the operator's executor and the web app's
  registry note (Settings, the sha256 of the registry in use);
* the web files against what the host serves (`index.html` carries the Content-Security-Policy).

Rust release binaries are reproducible only on an identical toolchain and host; the hashes say what was
built from the commit. CI (job `reproducible`) builds `web/dist` (with the kob-wasm bindings) and the
kob-executor release binary twice, from two checkouts at different paths, and fails when any file differs. The source-level artifacts (templates, record, registry) reproduce byte for byte
everywhere.

## 7. Upgrade order: indexers before wallets

Payload version 3 (compact placement records and in-place amends, `docs/spec/kob1-payload.md`) is written by
the current builders whenever a transaction carries an `ORDER` or `AMEND` record. An indexer built before
version 3 rejects every version-3 payload (`payload:unsupported KOB1 version 3`): it would list none of the
orders the new wallets place and would show an in-place amended order as open with an unknown state.

1. Operators upgrade their **executors / indexers** first (the database migrates to schema 4 in place, no
   replay; `docs/ops/executor.md` 7.6). Check `GET /v1/health` and that a version-3 placement made with
   `kob-cli` is listed.
2. Then the **web app** and other wallets (`kob-cli`, bots) are published with the new builders.
3. Announce the order in the release notes; a host serving the web app should point it only at indexers
   that report the release version.

Older version-2 payloads keep decoding (record logs replay unchanged), so a rollback of the wallets is safe;
a rollback of an indexer below version 3 is not once version-3 orders exist.

## 8. Operator requirements

| | Requirement | Basis (`docs/ops/executor.md`) |
|---|---|---|
| Node | rusty-kaspa v2.1.0, `--utxoindex --retention-period-days=7 --disable-upnp`, JSON wRPC on loopback (`:18110` mainnet), never `--unsaferpc`; official minimum hardware 8 cores, 16 GB RAM, 640 GB SSD (up to ~1.6 TB worst case at 7-day retention) | Part B 2, Part C "Node" |
| Placement | **the indexer runs on the node's host (loopback) or LAN**: a remote node is for development only | B 3 "Bandwidth" |
| Bandwidth | about **1.4 KB/s per tx/s** of chain load at `High` verbosity (1.35 MB/s at 1,000 tx/s, 4.2 MB/s = 34 Mbit/s at 3,000 tx/s); give the link 3x the peak to catch up after an outage | B 3 |
| CPU | 3 to 10 us of one core per accepted transaction of a realistic mix, plus 0.25 to 0.3 ms per KOB transaction (single-threaded database pass: about 3,500 KOB tx/s per core at the tip); 3 to 4 % of one core at 3,000 tx/s with the default mix | B 3 "Processing capacity" |
| Memory | 0.3 to 1 GB typical, budget up to 4 GB for full-size batches while catching up | B 3 |
| Disk (indexer) | about 1 KB of record log plus 2.6 KB of database per KOB transaction / order, 42 MB constant block window; the record log is the asset to back up off site | B 3, B 8 |
| Keys and funds | an operator hot key for `run` (matcher, keepers, facilitator) with a small working balance; `index` alone is keyless | Part C "Keys and funds" |
| Monitoring | `/v1/health`, textfile metrics, alerts on `gap`, lag and node version | B 6, A.4 |

An operator should also keep a standby node and the TN10 drill current (B 7.8), and upgrade nodes
TN10 first, then standby, then primary (B 2).
