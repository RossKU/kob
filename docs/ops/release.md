# KOB mainnet release checklist

What a release manager does to cut a mainnet release, and what an operator needs to run it. KOB ships
software only (ISC): the release is a commit, its reproducible artifacts and the hashes below; the
mainnet services (node, indexer, matcher, keepers, x402 facilitator) are run by community operators.

Nothing in KOB has been independently audited, and no external audit precedes the launch (maintainer decision
2026-10-12): mainnet launches as a canary, with a visible pre-audit notice in the web app (`SECURITY.md`). The order
contracts and the token programs this build supports were reviewed internally only, and nothing on chain can pause or
rescue an order. The launch therefore runs with a strict program allowlist (section 3).

## 1. No genesis step: the build is reproducible from source

No order template depends on a network or a genesis (a stop is armed by a fill in the same transaction, the touch
trigger), so there is no transaction to broadcast before the release: every order template is the reference artifact
of `contracts/artifacts`, the same on every network. A mainnet deployment is therefore a
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
2. KCC-20 template bytes pinned against the upstream specification (Last Call) and its conformance vectors
   (`crates/kob-tests/vectors`, `docs/spec/kcc-conformance.md`); the KCC-20 programs other than the two below stay
   `pending-review` (not supported for trading). The reference 3/3 (`kcc20-ref-3x3`, KOB's standard program) is `reviewed` by internal
   review only (maintainer decision 2026-10-12, canary launch), and so is the reference's published public-mint build
   (`kcc20-ref-public-mint`, maintainer decision 2026-10-13, the same permissionless KCC-20 policy).
3. Internal review: no open high or critical finding.
4. The TN10 soak completed by the operator, including the incident, `recover` and backup / restore drills
   (`docs/ops/executor.md` 7.8).
5. Legal review of the OSS-provider position.

## 3. Registry for launch

`registry/tokens.json` is the mainnet registry the release pins: a snapshot of facts and of the programs this build
supports, not a list of approved tokens. KOB makes no listing decision; curation (official status, anti-impersonation,
logos, warnings written by people) belongs to external community token lists (`registry/README.md`, "Curation is not
KOB's"). At launch:

* **Supported programs (the program allowlist):** the two KRON programs (`kron-2433`, `kron-2732`), reviewed with
  conditions, and the reference KCC-20 3/3 (`kcc20-ref-3x3`, the program `kob token issue` and the web app issue),
  reviewed internally (maintainer decision 2026-10-12): every token of these programs is tradable, with a registry entry
  or without one (issuance is permissionless; there is no listing step for any token). The reference's published
  public-mint build (`kcc20-ref-public-mint`, KOB's second issuance option and the program of third-party launchpads such
  as Kasseal) is reviewed internally too (maintainer decision 2026-10-13) and supported: its tokens are tradable the same
  way; a token whose genesis carries a `PublicMint` can grow up to `supply + mint_allowance` (recorded by verify-genesis,
  and reported as the warning `live_mint_authority` while a minter with allowance lives). The 8/8 prototype and KaspaCom's
  KCC20 0.2.5 (finding K-1) stay `pending-review`: this build does not trade their tokens on mainnet.
* **Token entries:** the eight KRON tokens of the census (KRON, KASCOV, PEPE, ANSEM, IFWEN, PEEPS, DNBT, KDIST) have
  entries with their identity check (`verified`) and genesis records (every genesis output an instance of the program on
  mainnet data, no live mint authority). An entry states facts only: it does not make a token tradable, and KOB says
  nothing about whether any token is genuine (PEPE's and DNBT's own names call them tests; that is their creators'
  statement, not KOB's). The web app shows no verification or officialness of any token: token issuance is
  permissionless and the UI does not curate tokens. It shows a short note on every token whose ticker another token it
  shows uses too.
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

# binaries: the deployment build records mainnet, refuses any other --network, and embeds the pinned registry;
# KOB_GIT_COMMIT (optional) is the commit /v1/health reports as `commit` (section 7); it is the only thing it changes
KOB_GIT_COMMIT=$(git rev-parse HEAD) cargo build --release --locked -p kob-executor -p kob-cli --features deploy-mainnet

# web app (static; network-independent bundle whose default network is mainnet; build:release ships the
# pinned registry/tokens.json as dist/registry/tokens.json, the bundle itself embeds no registry hash)
cd web && npm ci && npm run fetch-sdk && npm run build:wasm && npm run typecheck && npm test && npm run build:release && cd ..

# optional: node / bot bindings of the deployment variant (crates/kob-wasm/pkg-node-mainnet, pkg-mainnet)
scripts/build-wasm.sh --mainnet --web
```

`deploy-mainnet` and `deploy-tn10` are exclusive (a compile error names both). The templates are the same in
every build; the feature only records the network and pins the registry: `kob-executor` refuses a configured
network other than `mainnet`, and without `--tokens` it reads the embedded, pinned registry (its supported programs and token facts)
(an operator may still pass `--tokens` with an own list; the executor logs that its hash differs from the pin,
and `GET /v1/tokens` reports `registry.source` and `registry.sha256` either way).

The web bundle carries no network-dependent code (the browser build is the slim kob-wasm variant: no x402
payer entry point); its defaults are `network: mainnet` and `./registry/tokens.json`, which `npm run build:release`
ships in `dist/registry/` (the pinned file; a plain `npm run build` leaves the registry to the host). A hosting
operator may set `config.json` next to the app (`indexerUrl`, `nodeUrl`, ...). The project's own hosting (Cloudflare Pages from
`.github/workflows/deploy-pages.yml`, config from repository variables, post-deploy header and hash check, rollback) is
`docs/ops/web-hosting.md`; its public read server (`kob-executor index` behind Cloudflare Tunnel) is `docs/ops/chart-server.md`.

## 5. Tests before tagging

```
cargo fmt --all --check
cargo clippy --locked --all-targets -p kob-protocol -p kob-wasm -p kob-executor -p kob-cli -p kob-tests -p sil2argent -- -D warnings
KOB_SKIP_NETWORK_TESTS=1 cargo test --workspace --locked --no-fail-fast
cargo test --locked -p kob-protocol -p kob-executor --lib --features deploy-mainnet
scripts/build-wasm.sh --test
cd web && npm test && npm run e2e
node scripts/check-pins.mjs                                     # every 64-hex constant in the pinned docs and sources matches a generated artifact
scripts/build-deploy.sh testnet-10 --check && scripts/build-deploy.sh mainnet --check
cargo deny --locked check                                       # advisories, bans, licenses, sources (cargo-deny 0.20.2)
```

`node scripts/check-pins.mjs` fails when a 64-hex constant in a pinned file (the docs among them) is not a hash the generated
artifacts carry: do not paste a real covenant id, transaction id or hash into a document; write a placeholder such as `<64 hex>`.

CI runs all of these on every push (`.github/workflows/ci.yml`: the `deploy-mainnet` lib tests in job `test`), and
`scripts/release-hashes.sh` over the release bundle (`npm run build:release`) in job `web`.

## 6. Hashes to publish

```
scripts/release-hashes.sh target/release/kob-executor target/release/kob-cli
```

prints the commit, every order template hash of the record, the registry sha256 and the sha256 of
`contracts/SHA256SUMS`, the deployment record, every file of `web/dist` and the binaries given. It refuses
to run when the record does not reproduce, and when the web bundle does not ship the pinned registry as
`dist/registry/tokens.json` (the bundle embeds no registry hash: the app shows no registry identity).
Publish its output with the release (release notes, the operator announcement and an on-chain `KOB-REL`
anchor when one is made). Users and operators check:

* the template hashes against `contracts/deploy/mainnet/DEPLOYMENT.md` of the commit and the chain (an order's
  P2SH is the template over its state; kob-wasm `templates()` lists what a build embeds);
* the registry hash against `GET /v1/tokens` `registry.sha256` of the operator's executor and the sha256 of the
  `registry/tokens.json` the web host serves;
* the web files against what the host serves (`index.html` carries the Content-Security-Policy).

Rust release binaries are reproducible only on an identical toolchain and host; the hashes say what was
built from the commit. CI (job `reproducible`) builds `web/dist` (with the kob-wasm bindings) and the
kob-executor release binary twice, from two checkouts at different paths, and fails when any file differs. The source-level artifacts (templates, record, registry) reproduce byte for byte
everywhere.

**Prebuilt binaries for operators** (`.github/workflows/release.yml`; the operators' page is
`docs/ops/executor-quickstart.md`). The tag is `v` + the workspace version (`Cargo.toml` `[workspace.package] version`, which
`kob-executor` and `kob-cli` inherit, so their `--version` is the tag): bump it, with `Cargo.lock`, in the very commit that is
tagged; the workflow refuses any other tag before it builds (`node scripts/package-release.mjs --check-tag v<version>` checks it
locally). Pushing the tag `v<version>` builds `kob-executor` and `kob-cli` with `--release --locked`
for Windows x86_64 (`x86_64-pc-windows-msvc`, static C runtime), Linux x86_64 (`x86_64-unknown-linux-gnu`, on the
oldest supported Ubuntu runner for the widest glibc range) and Android arm64 (`aarch64-linux-android`, bionic, for Termux;
built with the runner image's NDK clang for API 24), each as the `deploy-tn10` and the `deploy-mainnet` build, and
checks that each binary refuses the other network (not the Android one, which cannot run on the x86_64 runner: it is only
checked to be an arm64 ELF). `scripts/package-release.mjs` packs each into
`kob-executor-<version>-<network>-<target>.zip` (Windows) or `.tar.gz` with `QUICKSTART.md`, `LICENSE`, `NOTICE` and a
`SHA256SUMS` of those files; the archive bytes depend only on the files and `SOURCE_DATE_EPOCH` (the commit time). The
release job attaches the six archives, the Android app and a `SHA256SUMS` of them all to a **draft** release of the tag: add the output of
`scripts/release-hashes.sh` (above, with the binaries of the archives) to its notes, then publish it. `workflow_dispatch`
runs the same builds as a dry run that only uploads workflow artifacts. The binaries and archives are not code-signed (the
Windows binary shows SmartScreen); the Android app is signed with the **release key** (below). No container image is
published (the repository's `Dockerfile` builds one from the source for operators who want a container, quickstart 6).

**The Android app in the release** (`android/`, built by `.github/workflows/android.yml`, which `release.yml` calls as the job
`android`): `kob-executor-<version>-testnet-10-android.apk`, built with the testnet-10 deployment build of the executor and
listed in the release's `SHA256SUMS` like the archives. `versionName` is the workspace version (the tag without the `v`);
`versionCode` is the commit time in minutes, so every later commit, and so every later release, is an update of the earlier
ones. It is signed with the Android release key when the repository secrets `ANDROID_KEYSTORE_B64`,
`ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS` and `ANDROID_KEY_PASSWORD` exist (made once with
`node scripts/make-android-keystore.mjs`, stored as secrets, backed up; quickstart 9, "Release signing"): a new APK then
installs over the old one and keeps the app's data, the operator key included. **Check before publishing the draft** that the
APK file name has no `-debug-signed` (the run warns and the draft's notes say so when the secrets were missing: such an APK
cannot be updated in place, and the fix is to set the secrets, delete the draft and re-run the tag's `release` workflow, not to publish it).
The workflow verifies the signature with `apksigner`, that the APK is not debuggable, and its `versionName` / `versionCode`,
and prints the signer certificate's SHA-256 in the run summary: it must be the same on every release.

## 7. Upgrade order: indexers before wallets

A release that changes the `KOB1` payload version (version 4 today, `docs/spec/kob1-payload.md`) must reach the indexers
before the wallets: an indexer refuses a payload version it does not know (`payload:unsupported KOB1 version N`), so it
would list none of the orders the new wallets place.

1. Operators upgrade their **executors / indexers** first. A database schema change says in the release notes whether
   the database is brought up to date in place or rebuilt with `kob-executor index replay` (`docs/ops/executor.md` 7.6).
   Check `GET /v1/health` and that a placement made with `kob-cli` is listed.
   `GET /v1/health` reports the build that answers: `version` (the crate version, the tag without the `v`), `commit` (the git
   commit of the build, `null` when the build did not say) and `payload_version` (the `KOB1` payload version it reads and
   writes). Compare them with the release: `curl -s https://<indexer>/v1/health | jq '{version, commit, payload_version}'`.
   The release workflow (`.github/workflows/release.yml`) and the `Dockerfile` (`--build-arg KOB_GIT_COMMIT=...`) set
   `KOB_GIT_COMMIT` to the tagged commit; a plain `cargo build` leaves `commit` null. The value is read with `option_env!`
   at compile time: there is no build script, no `git` call and no timestamp, so the same source, toolchain and
   `KOB_GIT_COMMIT` give the same binary bytes (a build with another commit differs in that string only; to compare a
   rebuild with a published binary, set the same value).

   **`version` is `0.0.0` until the release bump.** It is the workspace version (`[workspace.package] version` in the root
   `Cargo.toml`, which every crate inherits), and that stays `0.0.0` on `master` until the release manager sets the release
   version, in the commit that is tagged (the release commit; the bump is the release manager's decision and is not made on a
   feature branch). A binary built from `master` before the bump therefore reports `"version": "0.0.0"`, which is not a defect of
   the build: tell such builds apart by `commit` (it is the git commit the build was made from) and compare that with the commit
   you mean. Once the release is tagged, `version` is the tag without the `v` and step 3 below can compare it; before that,
   "`version` equals the release" cannot be checked, only `commit` and `payload_version`. `kob --version` reports the same
   workspace version (the web app shows its own package version, `web/package.json`).
2. Then the **web app** and other wallets (`kob-cli`, bots) are published with the new builders.
3. Announce the order in the release notes; a host serving the web app should point it only at indexers
   whose `/v1/health` reports the release `version` (after the bump; before it, the release `commit`) (and, for a payload version
   change, a `payload_version` at least the new one).

A release that changes what a wallet writes on chain for an indexer to read follows the same order: the genesis payload of a token
creation (`KOBG`, `docs/spec/token-metadata.md` section 3) is read by an indexer of this build or later, which lists the issued
outputs to the issuer's key in any browser; an older indexer ignores the payload (the token is created all the same), so the issuer's
outputs are listed only by indexers already upgraded.

The record log keeps every payload as it was, so a rebuilt database replays the old ones unchanged; a rollback of the
wallets is safe, a rollback of an indexer below the payload version in use is not.

## 8. Operator requirements

| | Requirement | Basis (`docs/ops/executor.md`) |
|---|---|---|
| Node | rusty-kaspa v2.1.0, `--utxoindex --retention-period-days=7 --disable-upnp`, JSON wRPC on loopback (`:18110` mainnet), never `--unsaferpc`; official minimum hardware 8 cores, 16 GB RAM, 640 GB SSD (up to ~1.6 TB worst case at 7-day retention) | Part B 2, Part C "Node" |
| Placement | **the indexer runs on the node's host (loopback) or LAN** for a serious operator or a public read server (the chain traffic is large and latency slows catching up). A small operator may use the public Kaspa nodes through the resolvers, a cross-checked pair (`executor-quickstart.md`, 2): slower, with a longer first catch-up, and not recommended for a server others rely on | B 3 "Bandwidth", Part C "Node" |
| Bandwidth | about **1.4 KB/s per tx/s** of chain load at `High` verbosity (1.35 MB/s at 1,000 tx/s, 4.2 MB/s = 34 Mbit/s at 3,000 tx/s); give the link 3x the peak to catch up after an outage | B 3 |
| CPU | 3 to 10 us of one core per accepted transaction of a realistic mix, plus 0.25 to 0.3 ms per KOB transaction (single-threaded database pass: about 3,500 KOB tx/s per core at the tip); 3 to 4 % of one core at 3,000 tx/s with the default mix | B 3 "Processing capacity" |
| Memory | 0.3 to 1 GB typical, budget up to 4 GB for full-size batches while catching up | B 3 |
| Disk (indexer) | plan on about 10 KB per order in the full index (soak: 5.4 to 5.7 KB of database and 3.9 KB of record log per order; the synthetic 2.6 KB + 1 KB is the lower bound), plus a 42 MB constant block window; a lean matcher store is capped at 500 MB and keeps no log; the record log is the asset to back up off site | B 3, B 8, A.1.2 |
| Keys and funds | an operator hot key for `run` (matcher, keepers, facilitator) with a small working balance; `index` alone is keyless | Part C "Keys and funds" |
| Monitoring | `/v1/health`, textfile metrics, alerts on `gap`, lag and node version | B 6, A.4 |

An operator should also keep a standby node and the TN10 drill current (B 7.8), and upgrade nodes
TN10 first, then standby, then primary (B 2).
