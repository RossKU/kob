# Incident response: a contract or app bug after the mainnet launch

What the founder can do when a bug is found in an order contract, a signing path of the web app or `kob`, or the public indexer
after mainnet is live. The hosted app has one built-in tool for this, the **incident banner** (section 3a): a warning on every page,
set through repository variables. It blocks nothing (founder decision 2026-11-23, section 8). Everything else in this page needs no
new code.

## 1. What is true on chain, whatever happens

* Funds in an order sit in its covenant UTXO. Only the contract's own rules move them. There is no operator key, no admin entry and
  no pause in any KOB contract (`SECURITY.md`, *Design notes*). Nobody, the founder included, can move, freeze or rescue an order.
* So a fund-moving contract bug cannot be stopped on chain. An attacker needs no KOB software: any Kaspa transaction that the
  contract accepts works. **The only protection for a resting order is its maker's cancel**, and the cancel works as long as the
  attacker has not spent the order first.
* The maker's cancel needs no indexer and no hosted app: `kob recover --from FILE --cancel` from a backup or export, with the
  maker's key, finds the order on the node and cancels it (a partly filled order: add `--amount-left`, the amount it has left), and
  `kob cancel --view FILE` cancels from a saved indexer view of the order's current UTXO (`crates/kob-cli/README.md`).
  `kob cancel <covenant id>` reads the order from an indexer. The web app also cancels from the node and the records kept in the
  browser when no indexer answers (`docs/ops/chart-server.md`, section 5), except a Collected order that already holds bought tokens:
  cancel that one with `kob recover --cancel`, which moves the bought tokens back with it.
* What the founder controls is the hosted web app (`kob.kaspa.ag`, the Pages project `kob-web`), the public indexer index-main
  (`kob-api.kaspa.ag`) and what is published in this repository and on X. Executors are run by others; the founder runs none.

## 2. Who decides, and the first 15 minutes

The founder decides alone (there is no second maintainer). Write down the time of each step; it is the timeline of section 7.

1. **Confirm and classify.** Which component: an order kind (which `.sil` and which template hash, `contracts/deploy/mainnet/DEPLOYMENT.md`),
   a signing path of the web app or `kob`, kobd (indexer or executor), or the host. Who can exploit it: anyone (a third party can
   take funds), only an executor, or only the user themself. Is it being exploited now (look at the affected orders on the
   explorer and in `GET /v1/orders`)?
2. **Decide what to say and when** (section 5). If funds can be taken by anyone and the bug is already public or being exploited,
   warn at once: makers must cancel. If it is not public and not exploited, a short warning without the detail ("cancel your
   resting <kind> orders now; details later") still lets makers act; the detail waits for the fix (section 6).
3. **Pick the actions** from the table of section 3, in its order.

## 3. What can be done

| # | Action | What it does | How | Time (honest) |
|---|---|---|---|---|
| 1 | **The incident banner** (section 3a) | Every visitor of the hosted app sees the warning at the top of every page | Set `KOB_PAGES_INCIDENT_MESSAGE` (and `_SINCE`, `_URL`), run *deploy-pages* on `master`, approve `production` | one full deploy run: to be measured in the drill (section 7) |
| 2 | **Warn users elsewhere** | Makers who do not open the app learn they must cancel | Post on X (the release channel, `docs/ops/release.md` 7a), a pinned GitHub issue, and a GitHub Security Advisory (draft first) | minutes |
| 3 | **Tell executor operators** | They pause or upgrade | Direct message to the operators you know; the same X post. They pause with their `--pause-file` (`docs/ops/executor.md`, Part C: it stops the matcher, the keepers and the x402 facilitator) | minutes to send; when they act is up to them |
| 4 | **Roll the hosted app back** (only if the bug came with a new app version) | Serves the previous deployment's files and `config.json` | Cloudflare > *Workers & Pages* > `kob-web` > *Deployments* > the older production deployment > *...* > *Rollback to this deployment* (`docs/ops/web-hosting.md`, section 8). Then stop the next push from deploying again: revert the commit, or clear `KOB_PAGES_PROJECT`. A rollback also drops a banner that the older deployment did not have: set the banner again afterwards | a few minutes; serves at once |
| 5 | **Take the hosted app off mainnet** (only for a signing bug of the app itself) | `kob.kaspa.ag` stops building mainnet transactions | Roll back to the last testnet-10 deployment (instant) and restore the variables (`docs/releases/mainnet-launch.md`, section 6). **This also removes the easiest cancel path**: do it only when the app itself is what loses funds; for a contract bug keep the app up so makers can cancel in it | a few minutes |
| 6 | **Point users at the cancel** | Makers cancel even if the app or index-main is down | Repeat the commands of section 1 in every post and in the banner's link | - |
| 7 | **index-main misbehaves** | Stops wrong data | Stop it (on the host). The app then shows "no indexer" and still cancels from the node; users can pick another indexer in Settings. Restart only on a build you trust; rebuild from the record log if the store is wrong (`kobd db replay`, `docs/ops/executor.md` 7.6) | minutes to stop; a replay of a 1.2 GB log took 44 minutes on TN10 |

What these actions cannot do: stop an attacker, stop placements from `kob`, from another host's deployment or from a self-built
app, or cancel an order for its maker.

### 3a. The incident banner

Built: `web/scripts/pages-config.mjs` (`incidentNotice`) writes `config.json` `incident` from three repository variables, and the
app (`web/src/config.ts` `parseIncident`, `web/src/ui/shell/Banners.tsx`) shows it (`docs/ops/web-hosting.md`, "The incident banner").

| Variable | Value |
|---|---|
| `KOB_PAGES_INCIDENT_MESSAGE` | the notice, **plain text**, at most **280 characters** (a longer one fails the run). Markup is shown as typed, never as HTML; control and invisible characters become spaces. Unset = no banner |
| `KOB_PAGES_INCIDENT_SINCE` | optional: when the incident began, an ISO date such as `2026-11-23T10:00Z` (shown in UTC) |
| `KOB_PAGES_INCIDENT_URL` | optional: a link to more, shown as "More". **Any** `https://` URL of a public host without a user name, password, query or fragment is accepted: put the advisory or the pinned issue here (`https://github.com/RossKU/kob/security/advisories/<id>`, `https://github.com/RossKU/kob/issues/<n>`) |

`_SINCE` or `_URL` without a message fails the run, as does a wrong date or link: fix the variable and run again. The result in
`config.json`:

```json
"incident": { "message": "A bug was found in the KobBid contract. Cancel your resting buy orders now.", "since": "2026-11-23T10:00Z", "url": "https://github.com/RossKU/kob/security/advisories/<id>" }
```

**To show it** (a variable change alone deploys nothing):

```
gh variable set KOB_PAGES_INCIDENT_MESSAGE -R RossKU/kob --body 'A bug was found in the KobBid contract. Cancel your resting buy orders now: Portfolio > Open orders > Cancel.'
gh variable set KOB_PAGES_INCIDENT_SINCE   -R RossKU/kob --body 2026-11-23T10:00Z
gh variable set KOB_PAGES_INCIDENT_URL     -R RossKU/kob --body https://github.com/RossKU/kob/security/advisories/<id>
```

then *Actions* > *deploy-pages* > *Run workflow* on `master`. Every site of the run gets the same banner (the public site and, when
it is deployed, the internal mainnet app).

**What the run is, and why it takes time.** A banner run is a full `deploy-pages` run of the `master` head:

1. job `config` checks the variables (a wrong banner value fails here, in seconds);
2. job `build` rebuilds the wasm and the app from source, then **waits for the `ci.yml` run of the same commit to have passed**.
   If the `master` head's CI failed or was cancelled, the banner run fails too: re-run the failed CI job until it passes (or use the
   hand deploy below). If CI of the head is still running, the banner waits for it;
3. job `deploy` waits for **your approval** of the `production` environment (*Review deployments*), then uploads;
4. job `verify` checks the live files.

It also deploys every commit on `master` that was not deployed yet, so keep `master` at a commit you would deploy. The time from
"set the variable" to "banner live" is **to be measured** in the drill (section 7) and written here.

**To end it:** `gh variable delete KOB_PAGES_INCIDENT_MESSAGE -R RossKU/kob` (and `_SINCE`, `_URL`), then run the workflow again.

**When CI is red or too slow:** the hand deploy of `docs/ops/web-hosting.md` section 6, with `KOB_PAGES_INCIDENT_MESSAGE` (and
`_SINCE`, `_URL`) exported beside every other `KOB_PAGES_*` variable of the site, so the hand deploy writes the right network's
`config.json` (on mainnet: `KOB_PAGES_NETWORK=mainnet`, `KOB_PAGES_NETWORKS`, `KOB_PAGES_HOST_NETWORKS`,
`KOB_PAGES_NETWORK_SWITCH=1`, as in `docs/releases/mainnet-launch.md` section 2), and the wrangler of `scripts/deploy` (never
`npx wrangler`) with a Pages-only token. It skips CI and the `production` approval: do it only from a clean checkout of a pushed
`master` commit, compare the printed `config.json` with the live one before uploading, and say in the post that it was a hand
deploy. It needs the wasm and web build on the deploying machine (tens of minutes).

**What the banner cannot do:**

| It can | It cannot |
|---|---|
| Show every visitor of the hosted app a warning, within one deploy run | Move, freeze, block or rescue any funds: nothing on chain changes |
| Link to the advisory or the incident issue | Stop the hosted app from placing orders of the affected kind: it blocks nothing and changes no order flow |
| | Stop placements from `kob`, from other hosts' deployments, from self-built apps or from raw transactions |
| | Reach CLI users, executor operators, or anyone who does not open the hosted app |
| | Cancel anything for a user: each cancel is still the maker's own signed transaction |
| | Appear faster than a full deploy run (build, CI wait, approval), or survive a rollback to a deployment built without it |
| | Stop an attacker, who needs no KOB software |

The link is not restricted to this repository: a stolen GitHub session that can set variables could set a false banner with a link
to another site. The `production` approval (`docs/ops/web-hosting.md` 4a) gates it like any deploy: read the run's `config.json`
summary before you approve.

### 3b. Stopping new placements of one kind needs code

* **A web change that stops offering the kind**, merged to `master`: `ci.yml` must pass on the commit (the full suite and the
  sharded e2e), then deploy-pages builds, waits for that CI run (at most two hours, then it fails), asks for the `production`
  approval and uploads. Expect an hour or more; a flaky test makes it longer (re-run the failed job). Set the banner in the same
  run.
* **The hand deploy** (`docs/ops/web-hosting.md`, section 6) when CI is red or too slow, as above.
* **A new `kob` / `kobd` release** for CLI users and executors (tag, `release.yml`, publish, `docs/ops/release.md` 4-6): the build
  itself plus the checks before publishing, then each operator upgrades at their own pace. The founder cannot make anyone upgrade.
* **A contract fix** is a template re-version: a new template, a new record, a release, and a retirement of the old template into
  the cancel-only table with a published migration window (`docs/spec/template-retirement.md`, `docs/ops/release.md` 7). The
  retirement makes the current app and CLI offer only the cancel for those orders. Days, not hours. Retiring a template with no
  replacement is not a path the code has today.

Time these steps in the drill (section 7) and write the measured numbers here.

## 4. Order of steps

1. Classify (section 2) and write the first post (section 5).
2. Set the banner variables and start the banner run (section 3a); while it builds, post the first post on X and in the pinned
   issue. Approve the `production` deployment when it asks (read its `config.json` first).
3. Send the same text to the executor operators you know, and ask them to pause if the bug is in what an executor does.
4. If the bug came with an app release: roll the app back (table, row 4), then set the banner again if the older deployment lacks
   it. If the app's signing itself loses funds: take it off mainnet (row 5). Otherwise keep it up.
5. Start the code path (section 3b: a web change, then a release, then a template re-version) and post the expected times.
6. Post an update at each step and when it is done; change the banner text with the same run when the advice changes. Keep the
   posts in a GitHub issue too, so there is one place to read them.
7. After: section 6, and clear the banner (section 3a).

## 5. What to tell users

Plain text, no blame, no promise the code cannot keep. Every post repeats how to cancel.

**First post (a contract bug):**

> KOB security notice. A bug was found in the `<kind>` order contract on mainnet. Orders of that kind can be taken by others. If you
> have a resting `<kind>` order, cancel it now: web app > Portfolio > Open orders > Cancel (or Cancel all), or from the CLI:
> `kob cancel <covenant id>`. Other order kinds are not affected as far as we know. Do not place new `<kind>` orders. Updates here
> and at <GitHub issue URL>. Nobody can freeze or move orders, including us: only your cancel protects your order.

**Banner text (a contract bug, at most 280 characters):**

> A bug was found in the `<kind>` order contract. Cancel your resting `<kind>` orders now (Portfolio > Open orders > Cancel) and do
> not place new ones. Nobody can freeze or move orders: only your cancel protects them.

**First post (an app signing bug):**

> KOB security notice. The web app at kob.kaspa.ag has a bug in `<what>`. We have taken it off mainnet. Your funds on chain are where
> they were. To cancel orders meanwhile use the CLI: `kob cancel <covenant id>` (`kob recover --from FILE --cancel` from a backup).
> Do not sign transactions from copies of the app. Updates at <GitHub issue URL>.

**All clear:** what was wrong, which orders and amounts were affected (from the chain), what changed (commit, release, template
hashes), and what users do now (for example, place again under the new templates within the migration window).

**Where:** the banner of the hosted app (section 3a), the founder's X account (the channel of `docs/ops/release.md` 7a), a pinned
GitHub issue, and the Security Advisory once it is public.

## 6. After the incident

* **Disclosure.** Publish the GitHub Security Advisory with the fix, the affected templates and versions, and credit to the
  reporter if they want it (`SECURITY.md`). Hold the detail while unpatched orders can still be taken, but never hold the warning to
  cancel.
* **Template retirement** as the follow-up of a contract bug (section 3b).
* **A report** in `docs/reports/` with the timeline, the measured times and what to change.

## 7. Drill (testnet-10, before the launch)

Once on kob-tn10:

1. Pick a kind, write the first post and post it to a test channel.
2. Set the banner variables, run *deploy-pages*, approve, and time it from the first `gh variable set` to the banner showing on
   `https://kob-tn10.kaspa.ag` in a fresh window. Clear it the same way and time that too.
3. Roll the Pages project back and forward, and note that the rollback drops the banner.
4. Take the site to the other network and back with the variables.
5. Time a web change from merge to live.

Record the times in section 3 and section 3a.

## 8. Decided: a banner only (2026-11-23)

An earlier proposal on this page added two more fields to `config.json` `incident`: a list of template hashes the hosted app would
refuse to place (`blockKinds`), and a Portfolio prompt to cancel the affected orders (`cancelPrompt`). The founder decided on the
banner only (`docs/security/REVIEW-2026-11-23.md`, *Decisions*; finding F-01): the hosted app shows a notice and changes no order
flow, so there is no switch that stops placing or cancelling, and the founder holds no control over which transactions the app
builds beyond what hosting the site already means (a rollback or taking it down).

The trade-off that decided it, kept for the record:

* A blocking switch is still a **control the operator holds and is expected to use**: having it may create an expectation, or a
  duty, to watch for bugs and to use it fast, and questions when it was used late or not at all.
* Blocking a template for everyone is an operator decision about which transactions the hosted app builds.
* The founder's position is that of an open-source software provider with no control over user funds; the banner does not change
  that. Ask the lawyer (launch checklist step 1) about the banner too, and whether a blocking switch should ever be added.

Without a blocking switch, sections 3 to 5 are the incident path: the banner warns, and the makers' own cancels protect their orders.
