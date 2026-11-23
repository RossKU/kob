# KOB 0.2.0, the first mainnet release (<date>)

> **DRAFT.** Version 0.2.0 is not cut yet. The workspace version is still `0.1.0`, and the tag `v0.1.0` (2026-10-28) is
> testnet-10 only. Fill in `<date>`, `<commit>` and the Hashes section when the tag `v0.2.0` is built
> (`docs/ops/release.md` 6).

Tag `v0.2.0`, commit `<commit>`. This note lists what an operator must act on since `v0.1.0`. The user-facing changes of
testnet-10 are in `tn10-2026-11-18.md`. The launch steps for the web app and index-main are in `mainnet-launch.md`.

## Get it and check it

- The release has one archive per network and target: `kobd-0.2.0-mainnet-<target>` (`kobd`, `kob`, `QUICKSTART.md`) and
  `kob-0.2.0-mainnet-<target>` (`kob` alone). Targets: `x86_64-pc-windows-msvc` (`.zip`), `x86_64-unknown-linux-gnu` and
  `aarch64-linux-android` (`.tar.gz`).
- Check each download against the release's `SHA256SUMS` and with
  `gh attestation verify <file> --repo RossKU/kob --signer-workflow RossKU/kob/.github/workflows/release.yml --source-ref refs/tags/v0.2.0`.
- The binaries are not code-signed. Windows shows SmartScreen.
- `kobd --version` must say `mainnet build`, the release commit and the template generation below.
- A mainnet build refuses any other network. Use the `testnet-10` archive for TN10.

## Upgrade order

Indexers and executors first, then wallets and bots (`docs/ops/release.md` 7).

1. Upgrade every `kobd` (indexer and executor). Migrate its store (below).
2. Check `curl -s https://<indexer>/v1/health | jq '{version, commit, payload_version, template_generation}'`. It must say
   `0.2.0`, `<commit>`, payload `4` and `<16 hex>`.
3. Then the web app, `kob` and bots.

An older indexer does not list what new wallets write (for example curve orders and the token genesis record).

## Operator changes

### The daemon is `kobd`

- In `v0.1.0` it was `kob-executor`. Its files are now `kobd.toml`, `kobd.log` and `kob-operator.key`.
- **Do:** rename `kob-executor.toml` and `kob-executor.log`. The old names are not read.

### `--node` is now `--extra-node`

- `--extra-node URL` adds a further node (also `KOB_INDEX_NODES`, or `[[nodes]]` in the config).
- The node itself is `--rpc-url` (`rpc_url` in the config). The default is `resolver`, a cross-checked pair of public nodes.
- A bare `--node` on a start or a database tool is a usage error: exit 2. A supervisor does not restart it.
- `kobd setup --node` and `kobd x402 --node` still name the node itself.
- **Do:** fix start scripts, units and supervisors before the upgrade.

### Store schema 14: `kobd db migrate`

This build reads schema 14 only. It refuses an older store and says to run `kobd db migrate`.

- **Do (offline):**
  1. Stop every writer of the data directory. The migration takes the writer lock.
  2. Back up `index.sqlite3` and the whole `records` folder.
  3. `kobd db info --data-dir <dir>` shows the schema.
  4. `kobd db migrate --data-dir <dir>`.
- From schema 13 it compacts to 14 directly.
- From schema 9 to 12 (and 5 to 8) it first builds schema 13 in `index.sqlite3.migrating13`, then 14 in
  `index.sqlite3.migrating`, then deletes the schema 13 file.
- Every table is read back and compared before the swap. A failed or stopped run leaves the old store untouched.
- The old file stays as `index.sqlite3.before-migrate` until you delete it. That is the way back.
- **Free disk:** the new store (about 3/4 of the old one), the temporary index files, and for a store older than 13 also the
  schema 13 file.
- Time: 163 s for a 3.2 GB TN10 store (2.81 GB after).
- Instead of migrating: `kobd db replay` rebuilds the store from the record log. Move the old `index.sqlite3` aside first.
- An Executor's lean store migrates in seconds. Deleting it and letting the fast start rebuild it works too.
- **An older build refuses a schema 14 store.** To go back, restore `index.sqlite3.before-migrate` and the log as it was.
- Details: `docs/ops/executor.md` 7.6a.

### The record log is zstd

- Sealed segments are now `seg-*.kobrec.zst` (about a quarter smaller than gzip). `kobd db migrate` compresses them as its
  last step (`--no-compress-log` leaves that out). `kobd db compact-log` converts old gzips next to a running indexer.
- **A build before 2026-11-19 cannot read `.zst` segments.**
- **Rollback:** stop the indexer, run `zstd -d --rm seg-*.kobrec.zst` in `records` (the `zstd` command-line tool, not part of
  KOB), and put back the newest segment you saved. There is no `kobd` command that decompresses.
- Details: `docs/ops/executor.md` 7.6c, `mainnet-launch.md` 7 *Rollback*.

### `archive_pruning` now defaults to `false`

- An indexer with `lean = false` now keeps every row. An existing store that pruned before **stops pruning and grows**.
- **Do:** to keep pruning, set `archive_pruning = true` in the indexer config (with `lean = false`).
  `spent_holdings_retention_days` (default 30) still applies.
- After the first pruning pass of a store that kept everything, run `kobd db vacuum` once (offline) to give the space back.
- `kobd db replay` follows the configured value.

### Rate limits for a public indexer

- The defaults (20/s, burst 60) refuse the web app's cold screens part-way.
- **Do:** for a public indexer, use at least this (`mainnet-launch.md` 5):

  ```toml
  [api.rate_limit]
  per_ip_rps = 200.0
  per_ip_burst = 3000
  global_rps = 2000.0
  global_burst = 4000
  per_site_rps = 400.0
  per_site_burst = 6000
  heavy_route_cost = 5
  ```

- Then `kobd config check --config <file>`: no `PROBLEM:` line.

### Logo reports and logo votes

- New: a wallet can vote thumbs-up or thumbs-down on a token's logo. The vote is a signed personal message: no transaction,
  no fee. The web app hides a logo by default once at least 3 addresses voted and the net score is -1 or lower. The viewer
  can show all logos.
- Anti-abuse: the voter needs KOB history in the store at least 24 h old, a proof of work (20 bits), and one vote per wallet per
  24 h. The indexer keeps every accepted vote with its signature. `GET /v1/logo-votes` gives totals, never voters.
- Reports of illegal images stay separate (`[logo_reports]`): they hide a logo at once until the operator reviews it.
- Both are off by default (the routes answer 404).
- **Do (public indexer):**

  ```toml
  [logo_reports]
  enabled = true
  origins = ["https://<your web host>"]

  [logo_votes]
  enabled = true   # empty origins: the [logo_reports] origins
  ```

- **With `cors_allow_origin = "*"` and no `origins`, no browser report or vote is accepted.** Requests without `Origin`
  (scripts, the GitHub form) are not affected.
- Other knobs: `[logo_votes]` `pow_bits`, `interval_secs`, `min_history_secs`, `per_client_per_hour` (20), `max_per_day`
  (20000); `[logo_reports]` `cooldown_secs`, `per_client_per_hour` (5), `max_per_day` (200), `max_pending` (20).

### Automatic creator lock: keepers release it

- New template `KobLockAuto`. A token creation can lock the creator's share until a date. After the date **anyone** may send
  the release. It can only pay the creator: the tokens, and the 1 KAS lock coin less at most 0.03 KAS.
- The web app's Create token now locks this way by default. Settings, *Manual unlock*, keeps the older signed lock.
  `kob token create --lock-until` does the same; `--lock-manual` gives the signed lock.
- `kobd --executor` keepers release due locks for a 0.01 KAS tip, 3 minutes after the lock time (the node's past median time
  lags the clock). The operator's smallest KAS input comes back with the tip. The fee comes from the lock coin, at most
  0.02 KAS and within `--fee-max-tx-kas`. A release the node did not take waits a minute.
- **Do:** nothing to turn it on. `--no-lock-release` turns it off.
- Anyone can also release by hand: `kob token unlock <token id>` (no key), or `--tip` with a key to take the tip. The web
  app's Portfolio has *Release now*.
- `GET /v1/creator-locks?due=<ms>` lists due locks.

### Template generation

- `template_generation` of this release: `<16 hex>`.
- No order template has changed since the TN10 switch of 2026-11-18 (generation `113cd9b8b119cdaf`). KobLockAuto is a new
  lock template and changes no order pin. Confirm before the cut.
- Mainnet has no KOB orders under older templates, so there is **no migration window on mainnet**.
- On TN10, orders under the templates of the cancel-only table (`docs/spec/template-retirement.md`) can only be cancelled by
  their makers. No keeper refunds or kills them.
- An executor with another generation does not see this release's orders. Compare `template_generation` after the upgrade.

## Changes for wallets, bots and CLI users

- **Direct fill.** A crossing order in the web app can fill from the book directly: the wallet takes the resting orders at
  their own prices, no executor, only the network fee.
  - A crossing limit and a FOK: *Fill now* by default. A GTC, GTD or day remainder rests as a plain limit. A FOK fills in
    one transaction or is refused.
  - IOC and market: sent to the executors by default (the covenant IOC and the auction). *Fill now* is one choice away.
  - Pair orders and ladders keep the auction.
- **`kob take --yes` needs `--token <covenant id>`** and a price bound (`--max-price` taking a sell, `--min-price` taking a
  buy). An order whose node-checked state trades another token is refused. Fix scripts that pass `--yes`.
- **`--lock-until`** is at most 10 years ahead (3,653 days). A unix time must be in milliseconds (13 digits).
- **Market-start guard.** A market or close sell, a buy worth 100 KAS or more, and a start far from the last fill need
  `--floor-price` (a sell) or `--ack-market-start`, also when the book is node-checked. The node proves the listed orders
  exist, not that the indexer hides none.
- `kob replace <id> --gtd WHEN` sets a new expiry, in place for a plain limit.
- `kob place` refuses an unlisted token whose decimals only the indexer names, unless `--decimals` gives the same value.
- `--key` was removed from every command (exit 2). Use `--key-file`, `--key-env` or `KOB_KEY`.

## Wallets tested

TN10 run of 2026-11-20 (`web/e2e-real/README.md`), app at master `63cf3743e`, versions pinned in `tools/wallet-gate`:

| Wallet | Version | Result |
|---|---|---|
| KasWare | 0.11.6 | every scenario PASS, `direct` fill included |
| Kaspire | 0.5.32 | every scenario PASS, `direct` fill included |
| Kastle | 2.61.0 | cannot connect (its provider script throws under the app's Trusted Types) |

Not in that run: the automatic lock of Create token and logo votes (both merged after it). They are covered by the mock
end-to-end tests only. Mainnet smoke run (`mainnet-launch.md` 8): `<txids and wallet versions>`.

## Known limits

- **No external audit.** The contracts, token programs and apps were reviewed internally only. Mainnet runs as a canary.
- **Nothing on chain can pause or rescue an order.** A maker's way out is always the cancel.
- **The founder runs no executor.** Executors are run by community operators. Until one runs, nothing fills market and IOC
  orders and no keeper refunds or releases locks. Direct fills, cancels and manual releases work without one.
- **The public indexer `https://kob-api.kaspa.ag`** runs on a single host behind a Cloudflare tunnel, with no uptime
  promise. The app and bots trust what it lists. Run your own indexer for anything that matters.
- **Expiry is soft.** After an order's expiry, conforming matchers stop filling it, but the chain cannot stop a fill until a
  refund spends the order.
- Bridged tokens (kzUSDC, kUSDT): KOB recognises the minter code, not the bridge or its vault. Only the covenant id tells
  instances apart.
- Kastle does not work with the app.
- v0.2.0 is the first mainnet build. A bad release is fixed forward with v0.2.1, not rolled back to an older mainnet build.

## Hashes

Paste the output of `scripts/release-hashes.sh target/release/kobd target/release/kob` (run with the binaries of the archives)
here: the commit, every order template hash, the registry sha256, the sha256 of `contracts/SHA256SUMS`, the deployment record,
the `web/dist` files and the binaries.

```
<release-hashes.sh output>
```
