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
| `KOB_PAGES_EXTRA_INDEXER_URLS` | no | further indexers to cross-check against, comma-separated `https://` URLs, at most 4 |
| `KOB_PAGES_NODE_URL` | no | a `wss://` wRPC node **with its path** (see "The node and the SDK resolver"); empty (default) = a public node the Kaspa SDK resolver names, reached over Borsh |
| `KOB_PAGES_EXPLORER_URL` | no | block explorer base, `https://...`; empty = kaspa.stream of the network |
| `KOB_PAGES_BRANCH` | no | the project's production branch, default `main` |

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

A node that does not answer is never "checking..." for ever: after about 25 seconds the status bar says "unreachable" and a red banner says
whether no node URL is set (use Settings > Node URL, or ask whoever hosts the page) or the URL set did not answer.

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
* **No default routing to an executor.** The app talks to an indexer (read only) and to Kaspa nodes; orders are covenants on
  chain that any executor may fill. The deployment names no executor, matcher or facilitator, and the host runs none.
* **Several indexers welcome.** `KOB_PAGES_EXTRA_INDEXER_URLS` should list independent third-party indexers when they exist;
  the app cross-checks them and says "single indexer, not cross-checked" otherwise. Users can choose their own in Settings.
* **No telemetry.** No analytics, no tracking scripts, no Cloudflare Web Analytics.
* **Risk disclosure stays.** The app's warnings and disclosures (token powers, unverified genesis, look-alike tickers, the
  order disclosure before signing) are never removed or softened by a deployment.
