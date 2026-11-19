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
3. index-main is synced and answers for both hosts:
   `curl -si -H "Origin: https://kob.kaspa.ag" https://kob-api.kaspa.ag/v1/health` returns 200, `access-control-allow-origin`, and `"network":"mainnet"`.
   Run it again with `Origin: https://kob-tn10.kaspa.ag`, and against `https://kob-api-tn10.kaspa.ag` (`"network":"testnet-10"`).
4. index-main has the mainnet rate limits of section 5. Apply them first: launch traffic arrives as soon as the deploy is live.
5. Release notes are ready. They must say: **`kobd --node` is now `--extra-node`** (a further node). The node itself is `--rpc-url` (`rpc_url`).
   A bare `--node` on a start is a usage error (exit 2).

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
`https://kob-api.kaspa.ag/v1/logo-reports`.

## 3. Order of steps

1. Apply the index-main rate limits (section 5), then restart index-main and check `/v1/health`.
2. Set the variables of section 2, all of them, before any run. A run with only some of them set deploys a mixed config.
3. *Actions* > *deploy-pages* > *Run workflow* on `master`. Then approve the `production` environment.
4. The job validates the config, builds with `KOB_NETWORK=mainnet`, uploads, and checks headers and files. Read the job summary: `config.json` must match section 4.
5. Do the checks of section 4. Then publish the release notes.

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

Today (`tools/soak/run/index-main/index.toml`, read-only check 2026-11-19):

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
