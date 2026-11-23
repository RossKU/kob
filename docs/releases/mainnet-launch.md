# Mainnet launch runbook (web app and index-main)

The launch is a change of repository variables on the existing Pages project `kob-web`, not a new site. No new project, no domain move; `kob-web`
stays after the launch. Background: `docs/ops/web-hosting.md` 4b and 4d.

After the launch:

- `kob.kaspa.ag` opens on mainnet (indexer `https://kob-api.kaspa.ag`, index-main). The header network switch is on, so testnet-10 stays reachable through it.
- `kob-tn10.kaspa.ag` opens on testnet-10 (indexer `https://kob-api-tn10.kaspa.ag`).
- If a visitor picks the host's default network, the stored choice is cleared, so the browser follows the host again.
- The title prefix ("Testnet · ") comes from the network in force at runtime, not from the build.
- Server URLs with a query string or fragment are refused, both at deploy time (`pages-config.mjs`) and in the app (`config.ts`).

## 1. Before the launch

1. The release gates of `docs/ops/release.md` section 2 hold.
2. The launch commit is on `master` (the `KOB_PAGES_NETWORK_SWITCH` passthrough in `deploy-pages.yml`) and CI is green. It is inert while the variable is unset.
3. **The deploy gate is set** (`docs/ops/web-hosting.md`, 4a): `production` has a required reviewer and the deployment branch `master` only,
   `master` and the `v*` tags are protected. With `KOB_PAGES_NETWORK=mainnet` the workflow refuses to deploy until it reads both settings
   (or, if GitHub refuses that read, until `KOB_PAGES_PRODUCTION_GATE_CONFIRMED=1` is set).
4. **index-main runs the release build** (section 7): schema 14, the release's `commit` and `template_generation` in `/v1/health`, the
   rate limits of section 5 and the logo tables of section 7, step 6.
5. **The host stays up** (section 9): never sleeps, the indexer and `cloudflared` start at boot, and one reboot brought everything back.
6. **The Cloudflare Cache Rule** of `docs/ops/chart-server.md` section 3 (*The Cache Rule*) is on for `kob-api.kaspa.ag` and its check
   shows `HIT` (or the rule was removed because the plan does not honour short TTLs). Without it every visitor's read reaches the indexer host.
7. index-main is synced and answers for both hosts:
   `curl -si -H "Origin: https://kob.kaspa.ag" https://kob-api.kaspa.ag/v1/health` returns 200, `access-control-allow-origin`, and `"network":"mainnet"`.
   Run it again with `Origin: https://kob-tn10.kaspa.ag`, and against `https://kob-api-tn10.kaspa.ag` (`"network":"testnet-10"`).
8. index-main has the mainnet rate limits of section 5. Apply them first: launch traffic arrives as soon as the deploy is live.
9. Release notes are ready (`docs/releases/mainnet-0.2.0.md`, draft). They must say, among the rest: **`kobd --node` is now `--extra-node`**
   (a further node). The node itself is `--rpc-url` (`rpc_url`). A bare `--node` on a start is a usage error (exit 2).
10. The launch-day docs change is ready: `docs/releases/launch-day-docs.patch` (README, SECURITY.md, executor-quickstart: "mainnet is
    not launched" becomes the launch wording). It is applied in section 3, step 6, not before.
11. The incident runbook (`docs/ops/incident.md`) is read, and its TN10 drill done.

## 2. Repository variables (set by hand; do not set them before step 1 holds)

```
gh variable set KOB_PAGES_NETWORK        -R RossKU/kob --body mainnet
gh variable set KOB_PAGES_INDEXER_URL    -R RossKU/kob --body https://kob-api.kaspa.ag
gh variable set KOB_PAGES_NETWORKS       -R RossKU/kob --body '{"testnet-10": {"indexerUrl": "https://kob-api-tn10.kaspa.ag"}}'
gh variable set KOB_PAGES_HOST_NETWORKS  -R RossKU/kob --body '{"kob.kaspa.ag": "mainnet", "kob-tn10.kaspa.ag": "testnet-10"}'
gh variable set KOB_PAGES_NETWORK_SWITCH -R RossKU/kob --body 1
gh variable set KOB_PAGES_PUBLIC_URL     -R RossKU/kob --body https://kob.kaspa.ag
```

Unchanged: `KOB_PAGES_PROJECT=kob-web`. `KOB_PAGES_MAINNET_*` stays unset.
`KOB_LOGO_REPORT_URL` (GitHub logo-report form) stays on the testnet-10 indexer until you decide which list it feeds. For mainnet, use
`https://kob-api.kaspa.ag/v1/logo-reports`, and only once index-main has `[logo_reports]` on with the origins of section 7, step 6
(otherwise it refuses every report from the app).
Only if a run says the `production` environment could not be read: `gh variable set KOB_PAGES_PRODUCTION_GATE_CONFIRMED -R RossKU/kob --body 1`,
after the settings of `docs/ops/web-hosting.md` 4a are done.

## 3. Order of steps

1. Upgrade index-main to the release (section 7), with the rate limits of section 5, then check `/v1/health`.
2. Set the variables of section 2, all of them, before any run. A run with only some of them set deploys a mixed config.
3. *Actions* > *deploy-pages* > *Run workflow* on `master`. Job `config` checks the `production` gate; job `build` builds with `KOB_NETWORK=mainnet`
   and waits for CI of the commit; then approve the `production` environment (*Review deployments*).
4. Job `deploy` uploads, job `verify` checks headers and files. Read the job summary: `config.json` must match section 4.
5. Do the checks of section 4, then the **mainnet smoke run** of section 8. Announce nothing before it passes. If it fails, roll back (section 6).
6. Apply the launch-day docs change and push it (no deploy runs for these files):
   `git apply docs/releases/launch-day-docs.patch && git rm docs/releases/launch-day-docs.patch`, review the diff, fill in the date, commit.
7. Publish the release (`docs/ops/release.md` 6) and its notes, then the X post with the hashes.
8. First 24 hours: watch 429s, `lag_seconds`, `cross_checked`, `node_alarms` and the disk of index-main; keep `docs/ops/incident.md` at hand.

## 4. Checks after the deploy

- `curl -s https://kob.kaspa.ag/config.json` returns:
  `{"network":"mainnet","indexerUrl":"https://kob-api.kaspa.ag","extraIndexerUrls":[],"nodeUrl":"","networks":{"testnet-10":{"indexerUrl":"https://kob-api-tn10.kaspa.ag","extraIndexerUrls":[],"nodeUrl":"","registryUrl":"./registry/testnet-10/tokens.json"}},"hostNetworks":{"kob.kaspa.ag":"mainnet","kob-tn10.kaspa.ag":"testnet-10"}}`.
  There must be no `lockNetwork`.
- `curl -sI https://kob.kaspa.ag/` returns the CSP with `frame-ancestors 'none'` and `x-frame-options: DENY` (the job checks this too). Trusted Types and CSP are unchanged.
- `https://kob.kaspa.ag` in a fresh private window:
  - the tab title has no "Testnet · ";
  - the network chip says mainnet;
  - the status strip says "following";
  - the market list shows mainnet tokens.
- Switch to testnet-10 with the header chip. The page reloads on testnet-10, the title starts "Testnet · ", and the book is the testnet-10 one.
  Switch back to mainnet. `localStorage['kob.settings']` then has no `network`, because the host's default clears the choice.
- `https://kob-tn10.kaspa.ag` in a fresh window opens on testnet-10 with the "Testnet · " title.
- `kob-web.pages.dev` opens on mainnet (`network`).
- In the first hour, look for HTTP 429s on index-main. A rise means the limits of section 5 are too tight for real traffic.

## 5. index-main rate limits

index-main's `index.toml` on 2026-11-19 (read-only check):

```
[api]
cors_allow_origin = "*"
trusted_proxies = ["127.0.0.1", "::1"]
client_ip_header = "cf-connecting-ip"
metrics = false
max_ws_per_ip = 100

[api.rate_limit]
per_ip_rps = 500.0
per_ip_burst = 1000
global_rps = 2000.0
global_burst = 4000
```

The IPv6 site bucket is not set, so it runs at the defaults (100/s, burst 300). That is lower than the per-IP limit, so an IPv6 user is throttled first by the site bucket.

### What the web app sends (measured 2026-11-19)

The web app ran against the mock stack (`web/e2e` mock) with a Playwright request counter. The setup:

- 31 tokens, 48 h of trades, one connected wallet with two orders.
- Indexer latency of 100 ms, with 50 ms jitter.
- Cost is the limiter's weight: `heavy_route_cost` = 5 for the scanning routes, 1 for the rest.
- "Burst" is the smallest `per_ip_burst` a bucket refilling at that rate needs to never refuse the screen.
- The node (wRPC) is not the indexer and is not counted.

| Screen (cold load, then 30 s idle) | requests | cost | avg cost/s | peak cost in 1 s | peak cost in 5 s | burst at 20/s | burst at 100/s | burst at 200/s |
|---|---|---|---|---|---|---|---|---|
| Market list | 38 | 166 | 5.5 | 145 | 165 | 142 | 57 | 20 |
| Token Trade page | 57 | 157 | 5.0 | 124 | 135 | 108 | 45 | 45 |
| Trade page, a fill every 3 s | 118 | 546 | 17.9 | 85 | 170 | 103 | 61 | 57 |
| Portfolio | 14 | 46 | 0.9 | 42 | 42 | 37 | 36 | 35 |
| Activity | 54 | 226 | 7.5 | 160 | 194 | 162 | 68 | 21 |
| 10 page switches in 10 s, then 10 s | 236 | 1032 | 50.1 | 228 | 596 | 837 | 165 | 85 |

What these numbers mean:

- **Idle.** On a quiet screen the app sends at most about 1.4 cost/s (health, book, trades).
- **Live market.** Every fill on the open market makes the app read the market again: books, trades, stats, depth, candles, quotes, and the wallet's token UTXOs and orders. That costs about 55 per fill, coalesced to at most one read a second. A busy market therefore costs a viewer 18 cost/s (a fill every 3 s), up to about 55 cost/s (fills every second).
- **Scaling with tokens.** The Market list reads `/v1/stats` once per listed token (5 each), and Activity reads `/v1/candles` once per token (5 each). Both happen again on every visit. At 100 tokens, a cold Market or Activity costs about 510.
- **No latency.** With no latency the same run peaks higher, because the list fetches 4 at a time: Market 165 in 1 s, switching 359 in 1 s and 1273 in total. A switching user then needs a burst of 1076 at 20/s.
- **The defaults are too low.** At the kobd defaults (20/s, burst 60), every cold screen except Portfolio is refused part-way. The app retries a 429 twice, then shows the panel as unavailable.

### Recommended for index-main (mainnet)

```
[api.rate_limit]
per_ip_rps = 200.0
per_ip_burst = 3000
global_rps = 2000.0
global_burst = 4000
per_site_rps = 400.0
per_site_burst = 6000
heavy_route_cost = 5
```

Keep `[api]` as it is: `trusted_proxies` with `cf-connecting-ip` gives the real client IP behind the tunnel, and `max_ws_per_ip = 100` covers NAT.

Why these values:

- **One user.** The worst user is on a market with a fill every second, about 55 cost/s. That leaves 3.6x margin under 200/s.
  A user switching fast through 31 tokens needs a burst of 85 at 200/s. At 100 tokens, about 3x that.
- **NAT'd users behind one IP.** Ten users on a busy market need about 180 cost/s. Ten users cold-loading Activity together need about 2300 tokens, which fits the burst of 3000.
- **IPv6.** `per_site_*` is twice the per-IP limit, so an IPv6 /48 is not throttled before its own clients are.
  Today the site bucket runs at the defaults (100/s, burst 300), and that is the real IPv6 limit on index-main.
- **One heavy IP.** At 200/s one IP takes at most 10% of `global_rps`; today, at 500/s, it can take 25%.
  Users within a quarter of their own rate (50/s) count as quiet and keep the reserved half of the global bucket.
- **Global.** `global_*` is unchanged (2000/s, what TN10 runs). Raise it only after a load test of the index-main host.
  At about 230 cost per cold screen, 2000/s serves about 8 cold loads a second; above that, pages load slower (429 and retry).
- **Watch after the launch.** If 429s show up for normal users, raise `per_ip_rps` first.
  Two app-side improvements would cut the cost of switching screens: caching per-token stats and candles across visits, and one batched stats read for the list.

How to apply:

1. Back up `index.toml`.
2. Replace its `[api.rate_limit]` section.
3. Run `kobd config check --config index.toml`. It must report no `PROBLEM:` line.
4. Restart index-main the way it is run.

## 6. Rollback

1. Cloudflare dashboard > *Workers & Pages* > `kob-web` > *Deployments*. Pick the last testnet-10 production deployment, then *...* > *Rollback to this deployment*.
   It serves at once on both hosts. From a PC, `npx wrangler pages deployment list --project-name kob-web` lists the deployments.
2. Restore the variables, so the next push to `master` does not deploy mainnet again:
   ```
   gh variable set    KOB_PAGES_NETWORK        -R RossKU/kob --body testnet-10
   gh variable set    KOB_PAGES_INDEXER_URL    -R RossKU/kob --body https://kob-api-tn10.kaspa.ag
   gh variable set    KOB_PAGES_PUBLIC_URL     -R RossKU/kob --body https://kob-tn10.kaspa.ag
   gh variable delete KOB_PAGES_NETWORK_SWITCH -R RossKU/kob
   gh variable delete KOB_PAGES_NETWORKS       -R RossKU/kob
   gh variable delete KOB_PAGES_HOST_NETWORKS  -R RossKU/kob
   ```
3. Visitors:
   - A visitor who stayed on mainnet on `kob.kaspa.ag` stored nothing, so they are back on testnet-10.
   - A visitor who switched `kob-tn10.kaspa.ag` to mainnet keeps that choice. On the pinned site their mainnet page has no servers, so they pick testnet-10 in Settings.
4. Rate limits: the old `index.toml` from the backup also works on mainnet. Restore it only if the new limits cause trouble.

### 6a. Rolling back a release of the binaries

A published `kobd` / `kob` release that turns out to be bad:

1. On GitHub, edit the release: tick *Set as a pre-release* (so it is no longer the latest) and put a warning at the top of its notes. Do not
   delete it or its tag: its hashes and attestations stay checkable, and operators can see what they ran.
2. Post on X (and in a GitHub issue): what is wrong, which version to run instead, and whether funds are at risk (`docs/ops/incident.md`).
3. Operators go back to the last good tag's archive. **v0.2.0 is the first mainnet release**: there is no earlier mainnet build to go back
   to, so a bad v0.2.0 is fixed forward with v0.2.1; until then operators stop (or pause, `--pause-file`) the executor and keep the indexer
   on v0.2.0 if only the executor is wrong.
4. A store that a release migrated is not readable by an older build (`kobd db migrate` keeps `index.sqlite3.before-migrate`, the way back),
   and a build before 2026-11-19 does not read zstd log segments (`zstd -d --rm seg-*.kobrec.zst` first). Going back means the old store
   and log as they were before the upgrade (section 7, *Rollback*).
5. The web app: roll the Pages project back (step 1 above) or revert the commit on `master`, which deploys again.

## 7. index-main upgrade (schema 9 -> 13 -> 14)

index-main ran build `7f5f307a` (2026-11-09), a **schema 9** store (9.5 MB, no orders on 2026-11-20) that knows 2 of the 16 current
templates. It must run the release build before the launch. `kobd db migrate` takes a schema-9 store to 14 in one offline run: it first
builds the schema-13 layout in `index.sqlite3.migrating13`, then schema 14 in `index.sqlite3.migrating`, checks it and swaps
(`docs/ops/executor.md`, 7.6a). The old store is never altered.

1. **The binary.** Download the mainnet Windows archive of the release (`kobd-<version>-mainnet-x86_64-pc-windows-msvc.zip`), check it
   against the release's `SHA256SUMS` and with `gh attestation verify` (`docs/ops/release.md` 6), and unpack it into a folder of its own
   under your user profile (for example `%LOCALAPPDATA%\kob-mainnet\`), not into a worktree: a clean-up must not delete the running binary.
   `kobd --version` must say `mainnet build`, the release commit and its `templates` generation.
2. **Stop index-main** the way it is run (and keep it from being restarted while you work).
3. **Back up** its data directory to another drive: `index.sqlite3` (a plain copy is consistent while it is stopped) and the whole
   `records` folder (the newest segment is the one that changes; the way back needs it as it was), and `index.toml`.
4. **Check the store**: `kobd db info --data-dir <data dir>` with the new build says schema 9 and network mainnet.
5. **Migrate**: `kobd db migrate --data-dir <data dir>`. Seconds for a store this small. It keeps `index.sqlite3.before-migrate`. Its last
   step compresses the sealed log segments as zstd (none yet on a log this small); `--no-compress-log` leaves that out.
   (Instead of migrating, `kobd db replay --data-dir <data dir>` rebuilds a schema-14 store from the record log; with no orders it is as
   quick. Move the old `index.sqlite3` aside first.)
6. **Configuration** (`index.toml`): the `[api.rate_limit]` of section 5, and the logo tables, so the app's reports and votes are taken:
   ```toml
   [logo_reports]
   enabled = true
   origins = ["https://kob.kaspa.ag", "https://kob-tn10.kaspa.ag"]   # the hosts whose pages may report (the second can switch to mainnet)

   [logo_votes]
   enabled = true                                                    # origins: empty = the [logo_reports] origins
   ```
   With `cors_allow_origin = "*"` and no `origins`, no browser report or vote is taken (`docs/api-reference.md`, `[logo_reports]`).
   Then `kobd config check --config index.toml`: no `PROBLEM:` line.
7. **Start** the new build with the same configuration and check:
   ```
   curl -s https://kob-api.kaspa.ag/v1/health | jq '{version, commit, payload_version, template_generation, state, cross_checked, node_alarms}'
   ```
   `version`, `commit` and `template_generation` equal the release (`kobd --version`), `state` becomes `following`. Then run it for at least
   three days on the resolver pair before the launch, watching `cross_checked`, `lies_total`, `windows_failed`, `node_alarms` and the lag.
8. Keep `index.sqlite3.before-migrate` and the backup until the canary has run a week; then delete `before-migrate`.

**Rollback** (the old build again): stop index-main; move the new `index.sqlite3` (with `-wal` / `-shm`) aside; rename
`index.sqlite3.before-migrate` to `index.sqlite3`; put the backed-up `records` folder back (delete the segments the new build wrote after it,
and decompress any `.zst` segment with `zstd -d --rm seg-*.kobrec.zst`); restore the old `index.toml` (an older build may refuse the new
keys); start the old binary. It re-syncs the blocks since the cut-over from the node, within the node's pruning window.

## 8. Mainnet smoke run (before the announcement)

Mainnet has never been signed on with the release. Before the announcement, with real wallets and small amounts, on the public site
right after the deploy (or before the flip on the internal mainnet app, `docs/ops/web-hosting.md` 4c):

1. With the CLI (the release's mainnet `kob`, a key of your own with a few KAS and a little of one listed token): place one small resting
   sell (`kob place limit --token <ticker> --side sell --amount <n> --price <p>KAS --yes`) for the wallets to fill. Note its covenant id.
2. **With KasWare** (the version the release notes name), then again **with Kaspire**, each from a fresh browser profile:
   - connect; the network chip says mainnet; the status strip says "following";
   - place a small limit buy far from the market (it rests), see it in Portfolio > Open orders and in the book;
   - cancel it; it is gone from the book after the next block and the KAS is back;
   - **Fill now** a small part of the CLI's sell of step 1 (a direct fill, no executor); the tokens arrive in the wallet and the fill is
     listed.
3. Optional, if the release ships the automatic creator lock as the default: create a test token with a lock whose unlock time is minutes
   away, and check that it is released (by a keeper or from Portfolio).
4. Cancel the CLI's order (`kob cancel <covenant id>`). Write the transaction ids and wallet versions into the release notes.

Any failure: do not announce; roll back (section 6) and follow `docs/ops/incident.md` if funds were at risk.

## 9. The host of index-main

index-main runs on a single host behind a Cloudflare tunnel. Before the launch (`docs/ops/chart-server.md`, section 4, *Sleep, updates,
reboots*):

1. **Power**: the indexer host must not sleep, hibernate or suspend its network while it serves the public API (on Windows: the sleep and display-off timers set to never on mains power; check them again after an OS or vendor power-profile change).
2. **Start at boot**: index-main as a scheduled task at boot, `cloudflared` as a service (`cloudflared service install`).
3. **Windows Update**: active hours set, no automatic restart outside them.
4. **Reboot once** and check that the tunnel, index-main and the TN10 indexer come back without a hand.
5. **Backups and monitoring** (`docs/ops/chart-server.md`, sections 4 and 5): a daily copy of `records` (and a `VACUUM INTO` of the store)
   to another machine, one restore tried; an external probe of `https://kob-api.kaspa.ag/v1/health` that pages you; a disk alarm.

## 10. Incidents

A contract or app bug after the launch, or index-main serving wrong data: `docs/ops/incident.md`.
