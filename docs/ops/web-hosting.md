# Hosting the web app on Cloudflare Pages

The web app (`web/`) is static files: HTML, JavaScript, CSS and two wasm modules. This page publishes it on Cloudflare Pages
(free plan) from this repository with `.github/workflows/deploy-pages.yml`, or by hand from a PC. The data it shows comes from an
indexer (`kob-executor index`; running one for the public: `docs/ops/chart-server.md`) and from Kaspa nodes.

**First setup, in order.** The workflow needs the public address of an indexer, and that address needs a domain: the order of the
whole first setup (domain, indexer, tunnel, then this page) is in `docs/ops/chart-server.md`, "Start here". Sections 1 to 3 below are
the Pages half of it.

Why Pages and not GitHub Pages: GitHub Pages may not be used for sites "primarily directed at facilitating commercial
transactions", and it cannot send the security headers of `web/public/_headers` (Content-Security-Policy with
`frame-ancestors 'none'`, `X-Frame-Options: DENY`, the signer page's own policy). Pages reads `_headers` from the upload.

## 1. Cloudflare account and Pages project

1. Create a Cloudflare account (free plan) at https://dash.cloudflare.com/sign-up and verify the e-mail address.
2. Note the **Account ID**: dashboard, *Workers & Pages*, right-hand column (or the URL `dash.cloudflare.com/<account id>/...`).
3. Create the Pages project as a **Direct Upload** project (not "Connect to Git": Cloudflare's build image cannot build the app,
   which needs Rust and wasm-bindgen; GitHub Actions builds it and uploads the result). From a PC with Node 22:

   ```
   npx wrangler login
   npx wrangler pages project create kob-web --production-branch=main
   ```

   `kob-web` is an example name (lower-case letters, digits and `-`): the app is then served at `https://kob-web.pages.dev`.
   In the dashboard instead: *Workers & Pages* > *Create* > *Pages* > *Upload assets*, name the project, upload any small folder
   (the first real deployment replaces it). Check the production branch under the project's *Settings* (`main` for a project
   created as above); the workflow must upload to that branch name (variable `KOB_PAGES_BRANCH`, default `main`), otherwise the
   upload becomes a preview and the workflow fails.
4. Do **not** enable Web Analytics for the project, and keep the features that rewrite pages off in a custom domain's zone
   (Rocket Loader, Email Address Obfuscation, Automatic HTTPS Rewrites): the app sends no telemetry, and the post-deploy check
   fails when a served file differs from the build.

## 2. API token

*My Profile* > *API Tokens* > *Create Token* > *Create Custom Token*:

* Name: `kob-pages-deploy`
* Permissions: **Account** > **Cloudflare Pages** > **Edit** (nothing else)
* Account Resources: *Include* > your account
* Optional: an expiry date, and *Client IP Address Filtering* left empty (GitHub's runners have no fixed addresses)

Copy the token once; Cloudflare does not show it again. It can deploy and delete Pages projects of the account and nothing
else. Revoke it on the same page if it leaks.

## 3. GitHub secrets and variables

Repository *Settings* > *Secrets and variables* > *Actions*.

**Secrets** (tab *Secrets*):

| Name | Value |
|---|---|
| `CLOUDFLARE_API_TOKEN` | the token of section 2 |
| `CLOUDFLARE_ACCOUNT_ID` | the Account ID of section 1 |

**Variables** (tab *Variables*; not secrets: they end up in the public `config.json`):

| Name | Required | Value |
|---|---|---|
| `KOB_PAGES_PROJECT` | yes | the Pages project name, e.g. `kob-web` |
| `KOB_PAGES_INDEXER_URL` | yes | the indexer the app reads, `https://...` (e.g. `https://chart.example.org`, `docs/ops/chart-server.md`) |
| `KOB_PAGES_NETWORK` | no | `testnet-10` (default) or `mainnet` |
| `KOB_PAGES_EXTRA_INDEXER_URLS` | no | further indexers, comma-separated `https://` URLs, at most 4: they cross-check the primary and take over its reads when it is down (see "Further indexers: cross-check and failover") |
| `KOB_PAGES_NODE_URL` | no | a `wss://` wRPC node **with its path** (see "The node and the SDK resolver"); empty (default) = a public node the Kaspa SDK resolver names, reached over Borsh |
| `KOB_PAGES_EXPLORER_URL` | no | block explorer base, `https://...`; empty = kaspa.stream of the network |
| `KOB_PAGES_TOKEN_NAMES_URL` | no | the source of token names and icons: the base of a kascov.io-shaped data set, `https://...` (default `https://kascov.io/data`); `off` = none (see "Token names and icons" below) |
| `KOB_PAGES_BRANCH` | no | the project's production branch, default `main` |

### Further indexers: cross-check and failover

The URLs in `KOB_PAGES_EXTRA_INDEXER_URLS` (`extraIndexerUrls` in `config.json`) do two jobs. List only indexers you trust to read the same
network; each is a `kob-executor` read API (`docs/api.md`).

1. **Cross-check** (always): the key facts a decision rests on (a token's powers, an open-list market's price scale, the best ask and bid, the
   start of a market order) are compared with the indexer in use; a disagreement is shown and a market order whose start is too far from another
   indexer's is refused.
   These are a second opinion on prices. They do not replace the node check: whatever indexers are configured, the top of the book a market or close
   order starts from is confirmed on the user's node before signing (`web/README.md`, *Book against the node*).
2. **Failover for reads**: when the primary (`KOB_PAGES_INDEXER_URL`) is unreachable, failing or not ready, the app reads the book, charts, tokens and
   orders from a healthy further indexer, shows the banner "Using backup indexer <host>", and goes back to the primary, dropping the banner, once the
   primary has answered well twice in a row. Nothing is built from the node: a node alone would give a partial book, so with no healthy backup the
   app shows the primary's own "indexer cannot be reached" / "behind the chain" banners as before.

How a backup is chosen (`web/src/data/indexer-failover.ts`):

* **Healthy** is what the header's status dot already means for the primary: `/v1/health` answers, `network` is the app's, and the state is `following` (or
  `catching_up` within the lag tolerance), at most 2 minutes behind. A backup must also report `payload_version` equal to the one this app build
  writes (4); an older executor without the field is not used. A backup on another network, of another payload version, lagging, catching up or
  starting is never used.
* **Preference**: a backup whose health says `book_complete: true` comes first, then one that does not report it, then one whose store started late
  (`book_complete: false`, `history_from_daa`). A late store can serve the live book, but charts, trades and older orders may be partial: the banner
  adds "History may be incomplete". Within a rank the list order decides, and the backup in use is kept (two equal backups do not take turns).
* **Retries are bounded**: a failing primary is re-read after 2 s and 4 s before it is given up (not when it has not answered well since the page
  opened: the page has nothing to show meanwhile); then every indexer is probed once. With nothing healthy it looks again after 5 s, 10 s, 20 s and so
  on, at most once a minute. On a backup the primary is probed every 15 s, backing off to a minute while it stays down, and not at all while the page is
  hidden. The status poll (`/v1/health` of the indexer in use, every 15 s) is the only steady load.
* **Cross-checking continues on a backup**: the primary (while it answers well) and the other further indexers are the references. One that is down,
  behind or on another network is left out until a probe shows it fit again, and the footer says "single indexer, not cross-checked" when none is left.
* **One indexer at a time**: a read goes to one indexer; the order environment (book, own orders, trades) of one ticket is read from the same one; when
  the indexer changes, the live feed reconnects to the new one and every open view reads again.
* **Which URLs**: only the primary and `extraIndexerUrls` of the merged configuration: the deployment's, or what the user saved in Settings (a banner
  names a saved list on every page; a link can never add one). A backup is never taken from a link or from an indexer's own answer.

Each backup serves the whole read load while the primary is down: give it the capacity of the primary (`docs/ops/chart-server.md`).

### The node and the SDK resolver

With `nodeUrl` empty the page asks the official Kaspa SDK for a node: `new Resolver()` with the network name (`web/src/data/node-rpc.ts`,
`doConnect`). It is the SDK's own and KOB does not change it: a built-in list of community resolver hosts (`*.kaspa.stream`, `*.kaspa.red`,
`*.kaspa-ng.org`, compiled into the SDK's wasm) is asked over HTTPS from the visitor's browser for a public wRPC node of that network, and the SDK
connects to the node it names. The code is the same on both networks (only `networkId` is `testnet-10` or `mainnet`); there is no
mainnet-specific path and no other fallback. Two things to know:

* The resolvers answer 404 for the JSON encoding and serve **Borsh** only, so the page asks for Borsh when the node comes from the resolver
  (since commit 3195209b; before it an empty `nodeUrl` left the page at "Node: unreachable" in a browser). That was verified live on testnet-10
  (2026-10-27); nothing in this repository's tests reaches a real resolver, and mainnet through the resolver has not been run. A public node
  is someone else's server: for a deployment others rely on, run a node of your own behind TLS and set `KOB_PAGES_NODE_URL`.
* A `wss://` node URL you set is the whole wRPC endpoint **including its path**, in JSON encoding. For a public node of the resolver family on
  testnet-10 that is `wss://<host>/kaspa/testnet-10/wrpc/json`; a bare `wss://<host>` answers 404 at the WebSocket handshake, and the page
  shows "Node: unreachable". `web/scripts/pages-config.mjs` prints a note when the URL has no `/wrpc/` path.

A node that does not answer is never "checking..." for ever: after about 25 seconds the header's status dot turns red ("Node: unreachable" when pressed) and a red banner says
whether no node URL is set (use Settings > Node URL, or ask whoever hosts the page) or the URL set did not answer.

### Token names and icons: an external dependency (kascov.io)

KOB keeps no token list of its own: not in the repository, not in the indexer. The market list is completed in the visitor's browser from a
third-party explorer, kascov.io (MIT, `github.com/Knitser/kascov`), which serves a static data set per network:

```
https://kascov.io/data/{mainnet|testnet-10}/tokens.json        the tokens (CORS *, Cache-Control max-age=30, no ETag or Last-Modified)
https://kascov.io/img/{mainnet|testnet-10}/<covenant id>        kascov's saved copy of a token's hash-committed picture (CORS *)
```

What the app takes from it, and what it does not (`web/src/data/kascov.ts`, `web/src/ui/market/kascov-merge.ts`):

* **Taken:** the creator's claimed ticker, name and image hash/URL (`claimed_*`), the declared decimals (only to flag a disagreement) and
  the program hash (`standard.view_template_hash`). **Never read:** kascov's `status`, `review`, `alive`, `invalid_reason`, `listed_*`.
  Supply, decimals used for trading, prices and orders stay the chain's and the KOB indexer's.
* **Which tokens:** one kascov knows and no KOB source lists is added (as a "no orders" row) only if the program hash it names is one of the
  programs this build pins (`crates/kob-protocol/data/programs.json`) and enables for the network: on mainnet a `reviewed` program, and the
  profile's `networks` (KForge: testnet-10 only; KaspaRocket: mainnet and testnet-10). The 46-byte KRON family has no hash in kascov's data set, so those
  tokens appear only through the registry or the indexer, as before.
* **Icons:** shown only when the fetched bytes hash (SHA-256 of the raw bytes, checked on real tokens of both networks) to the
  `claimed_image_hash`; otherwise a letter. The creators' own hosts (mostly `storage.googleapis.com`) send no CORS header, so the browser
  cannot read their bytes: the app fetches kascov's saved copy first (its `/img/` route is part of the kascov web app, not of its documented
  data API, so it is best effort; a 404 falls back to the creator's own https URL, which works only where that host allows CORS). The
  verified bytes are shown from a `blob:` URL; `img-src` is therefore `'self' data: blob:` and never names a host.
* **Failure:** unreachable, slow (8 s), not JSON, a changed shape, a file for another network, larger than 12 MB: the list is exactly what
  the registry and the indexer give, plus a one-line "unavailable" hint. Nothing is thrown into the UI and nothing is stored as authority
  (the only persistent item is the visitor's icon switch, `kob.icons`).

**Refresh policy** (one constant, `KASCOV_REFRESH_MS` = 120 s in `web/src/data/kascov.ts`; kascov rebuilds the file about every 2 minutes):

* fetched when a view that needs names or icons is shown (the market list, a token page); while that view stays visible, again at most every
  2 minutes; no polling on other pages, none while `document.hidden`, none after the view is left (coming back to a stale list fetches once);
* one in-flight request and one in-memory cache are shared by every component of the page; nothing is kept between page loads;
* after a failure the next try waits 30 s, then 60 s, doubling to 10 minutes, and the list already shown stays on screen;
* when `generated_at_ms` did not advance the parsed result is kept as it is and nothing re-renders;
* load per visitor with the list open: mainnet about 0.8 MB (about 120 KB gzipped) and testnet-10 about 2.4 MB per 2 minutes, plus the pictures
  of the rows scrolled into view (about 70 KB on average, 0.5 MB at most; at most 4 at a time, each fetched once per visit).

**The source is a host setting.** `KOB_PAGES_TOKEN_NAMES_URL` (config.json `tokenNamesUrl`, the variables table above): the base of the
data set, read as `<base>/<network>/tokens.json` in kascov's shape, `https://` only; unset or empty in the workflow = `https://kascov.io/data`;
`off` in the workflow (an empty `"tokenNamesUrl": ""` in a hand-written config.json) turns the feature off: no request is made and the
Settings toggle is disabled (GitHub cannot tell an empty variable from an unset one, hence the word). The saved pictures are read from `img`
beside the last segment of the base (`https://kascov.io/data` -> `https://kascov.io/img`). Only the deployment can set it: a link or a stored
setting is ignored. A host can put a caching proxy in front of kascov without a code change: serve the same two routes with
`Access-Control-Allow-Origin: *`, keep `Cache-Control` of 30 s or more, and do not rewrite the bodies (the app checks every picture against its
hash, but names are the creators' claims as the source gives them).

**CSP.** The shipped `connect-src` allows `https:`, so the default works as built. A deployment that **pins `connect-src` to exact origins**
(`web/README.md`, "Deployment headers") must add the origin of the token-names source (`https://kascov.io` by default, your proxy's
origin otherwise); without it the list silently shows no names or icons. The pictures of creators' own hosts are the fallback only and need
`https:` in `connect-src`; a pinned policy turns them into letters, which is safe. `img-src 'self' data: blob:` stays as shipped.

**Privacy.** The visitor's browser asks the source for the list (it sees the visitor's IP and that the market list was opened) and for the
pictures of the rows in view (kascov.io, or the creator's host when kascov has no copy). The founder accepted this for a DEX; there is no
proxy through the host. The visitor can switch pictures off in Settings ("Load token icons from third-party hosts", on by default): letters
only, and no image host is contacted. The host sees nothing of this.

**Neutrality.** The names are the creators' own claims, labelled "declared by its creator"; the list is not curated, ranked or marked
"verified" by KOB or by the host, and kascov's ratings are not used. A host that repoints `KOB_PAGES_TOKEN_NAMES_URL` to a curated source
changes what the page says about tokens: section 9 applies.

Every URL must be `https://` (a node `wss://`) on a public host, without user name, password, query or fragment: the page is
served over https, so a browser refuses `http://` and `ws://` from it, and `config.json` is public (never put an API key in a
URL). `web/scripts/pages-config.mjs` checks this before anything is built and fails the run with every problem listed.

**Changing a variable does not deploy.** The workflow runs on a push to `master` that touches the app (section 4) and on *Run workflow*;
editing a repository variable (`KOB_PAGES_NETWORK=mainnet`, a new `KOB_PAGES_INDEXER_URL`, ...) changes nothing until the next run.
After a change, open *Actions* > *deploy-pages* > *Run workflow* (branch `master`), approve the `production` deployment if reviewers are set
(section 4a) and check the result (section 7).

**Until the two secrets and the two required variables are set, the workflow does nothing**: job `config` writes "Cloudflare
Pages deploy skipped" and what is missing into the run's summary, and job `deploy` is skipped. CI stays green (forks too).

## 4. What the workflow does

On a push to `master` that changes `web/` or an input of the app (kob-wasm, kob-protocol, contracts, registry, the toolchain), and
on *Actions* > *deploy-pages* > *Run workflow*:

1. builds and tests the app exactly as the `web` job of `ci.yml` (kob-wasm slim bindings, the pinned kaspa-wasm SDK,
   typecheck, unit tests, `npm run build:release`, `scripts/release-hashes.sh`, the mock e2e suite);
2. writes `web/dist/config.json` from the variables; on `testnet-10` also `web/dist/registry/tokens.json` as the supported
   token programs with no token entries (the shipped registry names mainnet tokens; it is the same list `kob-executor setup`
   writes as `kob-tokens.json`), on `mainnet` the pinned registry of `build:release` stays. With no token entries the app names no
   token: it opens on the tokens that have a ticker or declared decimals, and lists the others (no ticker, no declaration) only
   behind "Show N token(s) without a name" on the market page (a search or a covenant id finds them anyway); a testnet-10 page can
   therefore open on "No tokens yet." while the indexer holds tokens;
3. uploads `web/dist` with Cloudflare's wrangler action (pinned by commit, wrangler version pinned) to the production branch;
4. writes the deployment URL, `config.json`, the sha256 of every deployed file and the output of `scripts/release-hashes.sh`
   into the run's summary;
5. checks the live deployment (`web/scripts/check-pages.mjs`): `/` sends the app's Content-Security-Policy (with
   `frame-ancestors 'none'`), `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`;
   `/signer.html` and `/signer` send the signer's own policy and `X-Frame-Options: SAMEORIGIN`, never the app's; `/assets/*`
   sends `Access-Control-Allow-Origin: *`; every built file is served byte for byte. Any failure fails the run.

One deployment runs at a time; a newer push cancels an older run. The job has read access to the repository only.

### 4a. The deploy gate

The signer page of this deployment is where users type private keys, so whoever can change what the workflow deploys can steal them.
Two guards are in the workflow, one setting is yours:

* **Only `refs/heads/master` deploys.** A run of any other ref (for example *Run workflow* with a branch selected) fails in the first
  step of job `config`, and job `deploy` also checks it. A push to a branch never deploys; the push trigger is `master` only.
* **Job `deploy` runs in the environment `production`** (`environment: production`). GitHub creates the environment the first time
  the workflow runs. Repository *Settings* > *Environments* > *production*: add **Required reviewers** (yourself, and a second
  person if there is one) so that every deployment waits for an approval in the run (*Review deployments*) before the job, and the
  Cloudflare upload, starts; under *Deployment branches and tags* choose *Selected branches and tags* and add `master`. With no
  reviewer the environment changes nothing: deployments run as before (the gate is then only the master check).
* Keep `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` as **repository secrets** (section 3, the names are unchanged). Job `config`
  reads whether they exist and has no environment, so secrets moved into the environment would look missing and the deploy would
  be skipped. Also useful, outside this repository: branch protection on `master` (pull request or at least no force push), two-factor
  authentication with a hardware key on the GitHub and Cloudflare accounts, and a Cloudflare token limited to *Account > Cloudflare
  Pages > Edit*.

The post-deploy check compares the deployment with the `dist` built in the same job, so it detects a bad upload, not a bad commit: the
reviewer approving a deployment is the one who vouches for the commit (the run names it, and the summary lists the sha256 of every
deployed file; `reproducible` in `ci.yml` builds the same `dist` twice).

The deployment does not set `quoteTokens` (no token is marked as a dollar) and keeps the scheme-only `connect-src` of
`_headers`: with the public resolver the node is any public Kaspa node the resolver names, and users may point the app at another indexer in
Settings, so the hosts cannot be pinned (README, *Deployment headers*).

## 5. Custom domain

Pages project > *Custom domains* > *Set up a custom domain*, e.g. `kob.example.org`. With the domain's DNS on Cloudflare the
record is created for you; elsewhere add a `CNAME kob.example.org -> kob-web.pages.dev` at your DNS provider. The certificate
is issued automatically (minutes). `_headers` applies to the custom domain as to `*.pages.dev`. Check it (section 7) with the
custom domain as the URL.

## 6. Deploying by hand from a PC

The same result without GitHub Actions (Node 22, the Rust toolchain of `rust-toolchain.toml`, Git Bash on Windows):

```
cd web && npm ci && npm run fetch-sdk && cd ..
bash scripts/build-wasm.sh --install-bindgen --web --slim
cd web && npm run build:wasm -- --copy && npm run typecheck && npm test && npm run build:release && cd ..
bash scripts/release-hashes.sh                       # the record reproduces, the bundle ships the pinned registry

# config.json (PowerShell: $env:KOB_PAGES_INDEXER_URL = 'https://chart.example.org'; ...)
export KOB_PAGES_NETWORK=testnet-10 KOB_PAGES_INDEXER_URL=https://chart.example.org
node web/scripts/pages-config.mjs --dist web/dist

npx wrangler pages deploy web/dist --project-name kob-web --branch main
node web/scripts/check-pages.mjs https://kob-web.pages.dev --dist web/dist
```

Deploy only from a clean checkout of a pushed commit (`git status` clean), so the files match what anyone can rebuild.

**Both networks.** One workflow run serves one network (one `KOB_PAGES_PROJECT`, one set of variables). To publish the testnet-10 and the
mainnet app side by side, deploy the second network by hand to a second Pages project (`wrangler pages project create` with another
name, then the commands above with its `KOB_PAGES_NETWORK`, `KOB_PAGES_INDEXER_URL` and `--project-name`) or fork the workflow; each needs its
own indexer (`docs/ops/chart-server.md`, section 7). Users who saved a network or indexer in Settings keep it; the app shows a banner for it.

## 7. Checking a deployment

* Headers and files: `node web/scripts/check-pages.mjs <url> --dist web/dist` (the build of the same commit, with the same
  `config.json`). Without `--dist` it checks the headers only.
* By hand: `curl -sI https://kob-web.pages.dev/` shows `content-security-policy: ... frame-ancestors 'none'` and
  `x-frame-options: DENY`; `curl -sIL https://kob-web.pages.dev/signer.html` ends with the signer policy
  (`sandbox allow-scripts; frame-ancestors 'self'`) and `x-frame-options: SAMEORIGIN`.
* The indexer, which `check-pages.mjs` never contacts (it checks headers and files only, and `pages-config.mjs` only the syntax of the
  URLs): after every deploy, and after every change of `KOB_PAGES_INDEXER_URL`, open the app and look at the status strip (it must say
  "following"), or by hand `curl -si -H "Origin: https://kob-web.pages.dev" https://chart.example.org/v1/health` must answer `200`, carry
  `access-control-allow-origin`, and show `"network"` equal to `KOB_PAGES_NETWORK`. A wrong host, a server without CORS or a down tunnel
  deploys green; the app then shows its "no indexer" or wrong-network banner.
* Token names: the page's request to the names source, in the browser's network tab, is `<tokenNamesUrl>/<network>/tokens.json` and answers
  `200` with `access-control-allow-origin`; `curl -s https://kob-web.pages.dev/config.json` shows `tokenNamesUrl` (absent = kascov.io). If you
  pinned `connect-src`, the origin must be in it (section 3). The app works without it, so a wrong source deploys green and the list simply
  shows no names.
* Hashes: the run's summary lists the sha256 of every deployed file; anyone can rebuild the commit and compare (`ci.yml` job
  `reproducible` checks that the bundle does not depend on the build directory). `curl -s https://kob-web.pages.dev/config.json`
  shows the configuration in force.

## 8. Rolling back

Pages keeps every deployment. Pages project > *Deployments* > the older production deployment > *...* > *Rollback to this
deployment*: it serves at once, with that deployment's files and `config.json`. The next push to `master` deploys again; to
stay on the old version, revert the commit (or clear `KOB_PAGES_PROJECT`, which makes the workflow skip). The list from a PC:
`npx wrangler pages deployment list --project-name kob-web`.

## 9. Neutrality rules for whoever hosts the app

* **No fee.** The hosted app adds no fee, tip or surcharge to any order beyond what the user sets in the ticket.
* **No ranking, no curation.** No token, market, indexer or executor is promoted, hidden, labelled "verified" or ordered by
  anything but the app's own neutral columns. `quoteTokens` and `home` are not used to favour a token.
* **Token names are the creators' claims.** The market list shows names, tickers and icons that a token's creator recorded on chain, as a
  third-party service passes them on ("Token names and icons" in section 3), labelled "declared by its creator". A deployment must not
  turn that into a recommendation: no "verified" or "official" marks, no ordering by it, no source that rates or hides tokens.
* **No default routing to an executor.** The app talks to an indexer (read only) and to Kaspa nodes; orders are covenants on
  chain that any executor may fill. The deployment names no executor, matcher or facilitator, and the host runs none.
* **Several indexers welcome.** `KOB_PAGES_EXTRA_INDEXER_URLS` should list independent third-party indexers when they exist;
  the app cross-checks them, reads from one of them when the primary is down ("Using backup indexer ..."), and says "single indexer, not
  cross-checked" otherwise. Users can choose their own in Settings.
* **No telemetry.** No analytics, no tracking scripts, no Cloudflare Web Analytics.
* **Risk disclosure stays.** The app's warnings and disclosures (token powers, unverified genesis, look-alike tickers, the
  order disclosure before signing) are never removed or softened by a deployment.
