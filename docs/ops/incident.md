# Incident response: a contract or app bug after the mainnet launch

What the founder can do, today and with no new code, when a bug is found in an order contract, a signing path of the web app or
`kob`, or the public indexer after mainnet is live. Section 8 is a **proposal** for a small incident switch in the hosted app; it is
not built.

## 1. What is true on chain, whatever happens

* Funds in an order sit in its covenant UTXO. Only the contract's own rules move them. There is no operator key, no admin entry and
  no pause in any KOB contract (`SECURITY.md`, *Design notes*). Nobody, the founder included, can move, freeze or rescue an order.
* So a fund-moving contract bug cannot be stopped on chain. An attacker needs no KOB software: any Kaspa transaction that the
  contract accepts works. **The only protection for a resting order is its maker's cancel**, and the cancel works as long as the
  attacker has not spent the order first.
* The maker's cancel needs no indexer and no hosted app: `kob cancel <covenant id>` with the maker's key, `kob cancel --view FILE`
  from saved indexer views, or `kob recover --from FILE --cancel` from a backup or export (`crates/kob-cli/README.md`). The web app
  also cancels from the node and the records kept in the browser when no indexer answers (`docs/ops/chart-server.md`, section 5).
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

## 3. What can be done with no new code

| # | Action | What it does | How | Time (honest) |
|---|---|---|---|---|
| 1 | **Warn users** | Makers learn they must cancel | Post on X (the release channel, `docs/ops/release.md` 7a), a pinned GitHub issue, and a GitHub Security Advisory (draft first) | minutes |
| 2 | **Tell executor operators** | They pause or upgrade | Direct message to the operators you know; the same X post. They pause with their `--pause-file` (`docs/ops/executor.md`, Part C: it stops the matcher, the keepers and the x402 facilitator) | minutes to send; when they act is up to them |
| 3 | **Roll the hosted app back** (only if the bug came with a new app version) | Serves the previous deployment's files and `config.json` | Cloudflare > *Workers & Pages* > `kob-web` > *Deployments* > the older production deployment > *...* > *Rollback to this deployment* (`docs/ops/web-hosting.md`, section 8). Then stop the next push from deploying again: revert the commit, or clear `KOB_PAGES_PROJECT` | a few minutes; serves at once |
| 4 | **Take the hosted app off mainnet** (only for a signing bug of the app itself) | `kob.kaspa.ag` stops building mainnet transactions | Roll back to the last testnet-10 deployment (instant) and restore the variables (`docs/releases/mainnet-launch.md`, section 6). **This also removes the easiest cancel path**: do it only when the app itself is what loses funds; for a contract bug keep the app up so makers can cancel in it | a few minutes |
| 5 | **Point users at the cancel** | Makers cancel even if the app or index-main is down | Repeat the commands of section 1 in every post | - |
| 6 | **index-main misbehaves** | Stops wrong data | Stop it (on the host). The app then shows "no indexer" and still cancels from the node; users can pick another indexer in Settings. Restart only on a build you trust; rebuild from the record log if the store is wrong (`kobd db replay`, `docs/ops/executor.md` 7.6) | minutes to stop; a replay of a 1.2 GB log took 44 minutes on TN10 |

What these actions cannot do: stop an attacker, stop placements from `kob`, from another host's deployment or from a self-built
app, or cancel an order for its maker.

**Stopping new placements of one kind needs code** (today there is no switch, section 8):

* **A web change that stops offering the kind**, merged to `master`: `ci.yml` must pass on the commit (the full suite and the
  sharded e2e), then deploy-pages builds, waits for that CI run (at most two hours, then it fails), asks for the `production`
  approval and uploads. Expect an hour or more; a flaky test makes it longer (re-run the failed job).
* **The hand deploy** (`docs/ops/web-hosting.md`, section 6) when CI is red or too slow: it skips CI and the `production`
  approval, so do it only from a clean checkout of a pushed commit, and say so in the post. It needs the wasm and web build on the deploying
  machine (tens of minutes) and a wrangler login.
* **A new `kob` / `kobd` release** for CLI users and executors (tag, `release.yml`, publish, `docs/ops/release.md` 4-6): the build
  itself plus the checks before publishing, then each operator upgrades at their own pace. The founder cannot make anyone upgrade.
* **A contract fix** is a template re-version: a new template, a new record, a release, and a retirement of the old template into
  the cancel-only table with a published migration window (`docs/spec/template-retirement.md`, `docs/ops/release.md` 7). The
  retirement makes the current app and CLI offer only the cancel for those orders. Days, not hours. Retiring a template with no
  replacement is not a path the code has today.

Time these steps in the drill (section 7) and write the measured numbers here.

## 4. Order of steps

1. Classify (section 2) and write the first post (section 5). Post it.
2. Send the same text to the executor operators you know, and ask them to pause if the bug is in what an executor does.
3. If the bug came with an app release: roll the app back (table, row 3). If the app's signing itself loses funds: take it off
   mainnet (row 4). Otherwise keep it up.
4. Start the code path (a web change, then a release, then a template re-version) and post the expected times.
5. Post an update at each step and when it is done. Keep the posts in a GitHub issue too, so there is one place to read them.
6. After: section 6.

## 5. What to tell users

Plain text, no blame, no promise the code cannot keep. Every post repeats how to cancel.

**First post (a contract bug):**

> KOB security notice. A bug was found in the `<kind>` order contract on mainnet. Orders of that kind can be taken by others. If you
> have a resting `<kind>` order, cancel it now: web app > Portfolio > Open orders > Cancel (or Cancel all), or from the CLI:
> `kob cancel <covenant id>`. Other order kinds are not affected as far as we know. Do not place new `<kind>` orders. Updates here
> and at <GitHub issue URL>. Nobody can freeze or move orders, including us: only your cancel protects your order.

**First post (an app signing bug):**

> KOB security notice. The web app at kob.kaspa.ag has a bug in `<what>`. We have taken it off mainnet. Your funds on chain are where
> they were. To cancel orders meanwhile use the CLI: `kob cancel <covenant id>` (`kob recover --from FILE --cancel` from a backup).
> Do not sign transactions from copies of the app. Updates at <GitHub issue URL>.

**All clear:** what was wrong, which orders and amounts were affected (from the chain), what changed (commit, release, template
hashes), and what users do now (for example, place again under the new templates within the migration window).

**Where:** the founder's X account (the channel of `docs/ops/release.md` 7a), a pinned GitHub issue, and the Security Advisory once it
is public. The app has no banner today (section 8).

## 6. After the incident

* **Disclosure.** Publish the GitHub Security Advisory with the fix, the affected templates and versions, and credit to the
  reporter if they want it (`SECURITY.md`). Hold the detail while unpatched orders can still be taken, but never hold the warning to
  cancel.
* **Template retirement** as the follow-up of a contract bug (section 3).
* **A report** in `docs/reports/` with the timeline, the measured times and what to change.

## 7. Drill (testnet-10, before the launch)

Once on kob-tn10: pick a kind, write and post the first post to a test channel, roll the Pages project back and forward, take the
site to the other network and back with the variables, and time a web change from merge to live. Record the times in section 3.

## 8. PROPOSAL (not implemented): an incident switch in the hosted app

**Status: a proposal for the founder to decide. Nothing of this section is built.** It closes the gap of section 3: today the hosted
app can warn no one and refuse nothing without a code change and a full CI run.

### What it would be

One optional field in the app's `config.json`, written by `web/scripts/pages-config.mjs` from one repository variable,
`KOB_PAGES_INCIDENT` (JSON), and validated there like the other variables:

```json
"incident": {
  "message": "A bug was found in the KobBid contract. Cancel your resting buy orders now.",
  "url": "https://github.com/RossKU/kob/security/advisories/<id>",
  "blockKinds": ["<64 hex template hash>"],
  "cancelPrompt": true
}
```

* `message`: plain text, at most 280 characters, shown as a red banner on every page of the hosted app (no markup, no link in the
  text).
* `url`: optional, one https link under `https://github.com/RossKU/kob/` (the advisory or the incident issue), so a hijacked
  variable cannot point users at another site.
* `blockKinds`: template hashes (or kind names) of the pinned record. The app refuses to plan a placement of those templates (an
  issue such as `incident-blocked` in the plan, the order button disabled with the message). Only order templates can be listed:
  never a token, an address or a user (the neutrality rules, `docs/ops/web-hosting.md` section 9).
* `cancelPrompt`: Portfolio shows "Cancel all affected orders" for the maker's orders under the listed templates. The maker
  still reviews and signs each cancel in their wallet.

**How it is set:** the founder sets `KOB_PAGES_INCIDENT` and runs *Actions* > *deploy-pages* > *Run workflow* on `master`. The run
builds again; its CI wait returns at once because the `master` head already passed; it asks for the `production` approval and
uploads. Clearing the variable and running it again removes the banner. A drill on kob-tn10 measures how long the banner takes to
show (the build job plus the upload; to be measured). A faster variant uploads the last good `dist` with only `config.json` changed;
that is more workflow code and is left out of this first sketch.

**What would change:** `web/src/config.ts` (the field, from `config.json` only, never from Settings or the URL), the placement
planner (the refusal), a banner and the Portfolio prompt, `web/scripts/pages-config.mjs` (validation), `deploy-pages.yml` (pass the
variable), and tests: a unit test "incident config blocks the listed kinds" and an e2e test for the banner and the prompt. Effort:
medium.

### What it can and cannot do

| It can | It cannot |
|---|---|
| Show every visitor of the hosted app a warning within one deploy | Move, freeze, block or rescue any funds: nothing on chain changes |
| Stop the hosted app from building new placements of a listed template | Stop placements from `kob`, from other hosts' deployments, from self-built apps or from raw transactions |
| Offer each maker a one-click way to cancel their affected orders | Cancel anything for a user: each cancel is still the maker's own signed transaction |
| | Stop an attacker, who needs no KOB software |

A user can always cancel from the CLI (`kob cancel`, `kob recover --cancel`), with or without the hosted app and the indexer.

### The trade-off

The founder's position is that of an open-source software provider with no control over user funds. This switch does not change
that: it acts only on the founder's own copy of the front end, the same control the founder already has by hosting the site at all
(a rollback or taking it down are stronger versions of it). But it is still a **control the operator holds and is expected to
use**:

* Having it may create an expectation, or a duty, to watch for bugs and to use it fast, and questions when it was used late or not
  at all.
* Blocking a template for everyone is an operator decision about which transactions the hosted app builds. Keep it to template
  hashes, never tokens or users, and record every use.
* A stolen GitHub session could set a false banner. The `production` approval (`docs/ops/web-hosting.md` 4a) gates it like any
  deploy, and the link is restricted to this repository.

This is a legal question as much as a technical one: ask it in the lawyer consult (launch checklist step 1) before deciding to build
it. Without it, sections 3 to 5 are the incident path.
