# KOB 0.2.0, the first mainnet release (<date>)

> **DRAFT.** Version 0.2.0 is not cut yet. The workspace version is still `0.1.0`, and the tag `v0.1.0` (2026-10-28) is
> testnet-10 only. Fill in `<date>`, `<commit>` and the Hashes section when the tag `v0.2.0` is built
> (`docs/ops/release.md` 6).

Tag `v0.2.0`, commit `<commit>`. This note lists what an operator must act on since `v0.1.0`. The user-facing changes of
testnet-10 are in `tn10-2026-11-18.md`. The launch steps for the web app and index-main are in `mainnet-launch.md`.

**x402 is experimental.** x402 (the `kob-x402` crate, the facilitator in `kobd`, the client SDK `packages/kob-x402` and `docs/spec/x402-*.md`) is outside the mainnet launch review of 2026-11-23 and has no payment UI in the web app (it only has a read-only watch of an invoice's status, in the notification panel); it gets a dedicated audit of its own before it is recommended for real funds or its KCC-20 profile is proposed as a standard. Open findings: [docs/security/x402-open-findings.md](../security/x402-open-findings.md). Nothing in this release asks anyone to use it with real funds.

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

### First start of this build: the live books are filled once

- The first start over a store that an earlier build wrote (also after `kobd db compact`, and after a store went back to an
  older build and returns) fills the live-book tables (`book_live`, `pair_live`) from the live listed orders and sets every
  row's fillable window, in one transaction, before the API listens. Later starts only check them (about 0.1 s).
- Time, measured on a synthetic store of 100,000 live listed orders on the build PC (Windows, the test profile `opt-level = 1`, so a release
  build is faster): 22 s, about 0.2 ms per live order. The TN10 soak copy (320 live orders, `docs/ops/executor.md` 5.5)
  takes well under a second. The earlier code set each row in a commit of its own, 0.8 ms per row measured (80 s for
  100,000) before the API listened (R5F-2). While it runs
  the API is not up: the web goes to standalone mode and an Executor plans nothing.
- This build's book rules are version 6: the first start decides every live-book row again (the time above). It also marks
  once which stored fills make a price (`order_events.priced`, one pass over the fills) and counts the KAS locked in live
  orders (`counters.kas_locked`); triggers keep both from then on. `/v1/metrics` gains
  `kob_api_read_budget_refused_total{route}` and `kob_live_book_undecided_rows_total` (rows whose decision failed: not open,
  the commit goes on).
- The same happens once more, at the first commit, when the listing minimum (`min_order_value_sompi`) or the book rules
  change: every row is decided again in that one transaction.
- **Going back to an earlier build is not supported once this build has opened a store.** An earlier build runs on it
  (schema 14; the new tables and triggers stay), but it decides the live-book rows it touches by its own rules and leaves
  them so, and this build does not see that it did: dust and orders no fill can take may then hold book places, and pair
  orders may be missing from the pair books, until each row's next change. Its `kobd db compact` also refuses the store
  ("the old store has a table this build does not know"). **Do:** keep the store from before the upgrade. `kobd db migrate`
  from the deployed schema 13 writes a new file and keeps the old one as `index.sqlite3.before-migrate` in the data
  directory: to go back, stop the indexer and put that file back as `index.sqlite3` (or rebuild with `kobd db replay`).
  If an earlier build did run on a store of this build, stop it and, before this build starts again, delete the one meta
  key that says which rules decided the rows: `sqlite3 <data-dir>/index.sqlite3 "DELETE FROM meta WHERE k = 'book_live_rules'"`.
  The next start then decides every live-book row again (the first-start time above).

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
- Anti-abuse: an address counts only if its first KOB history in the store is at least 7 days old (`min_history_secs`,
  604800) **and** earlier than the token's creation, so keys made for a vote, or after the token, do not count. It also needs a
  proof of work (20 bits), and one vote per wallet per 24 h. The indexer keeps every accepted vote with its signature. `GET /v1/logo-votes` gives totals, never voters.
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
- **The generation changes.** No order template has changed since the TN10 switch of 2026-11-18 (generation
  `113cd9b8b119cdaf`), but the generation now also hashes the automatic creator-lock template (KobLockAuto,
  `crates/kobd/src/version.rs`), so this release has a new value. **Every executor and indexer must run the release build (a
  roll is required)**, on TN10 too: a build with the old generation is not the release (its `/v1/health` and
  `kob_build_info` differ from the release's), and a build from before KobLockAuto neither indexes nor releases the automatic
  creator locks.
  Confirm the value before the cut.
- Mainnet has no KOB orders under older templates, so there is **no migration window on mainnet**.
- On TN10, orders under the templates of the cancel-only table (`docs/spec/template-retirement.md`) can only be cancelled by
  their makers. No keeper refunds or kills them.
- `template_generation` hashes the lock template too, so it differs from `113cd9b8b119cdaf` although no order template changed.
  An executor with other **order** templates does not see this release's orders: compare `template_generation` after the
  upgrade, and on a mismatch the `templates` of `deployment.json`.

### Behaviour changes

- **Maintenance no longer sells by default.** The operator's own token UTXOs are only merged; `--dust-sell` (also
  `KOB_DUST_SELL=1`, which also takes `true`, `yes` or `on`, or `[launch] args = ["--dust-sell"]`) turns the sale on, which accepts that its price floor comes from a
  book anyone can post into. `--no-dust-sell` is still accepted and does nothing.
- **The indexer judges the node it follows by the clock.** Whether the chain authority keeps up is decided by the cursor
  block's verified header time against the host's wall clock alone, judged every second (also while the node is
  answering): within 90 s it keeps up; above that its lag must fall by
  90 s in every 3 minutes, and once it has kept up it may spend at most 5 minutes in an hour above 90 s, or another node
  takes over (with a single node, `/v1/health` says so). Only the time kobd waits on that node counts: its own database
  work, a full disk or a slow cross-checking node is not the node's; which time is whose comes from kobd's own code, never
  from the text of a node's answer, and a stretch above 90 s that lasts 30 minutes by the wall clock without the lag
  falling by 90 s fails whoever's time it was. A window whose transactions use many subnetwork lanes is proven
  whatever their number (in pieces of at most 4,096 lanes, one chain block always whole): before, more than 64 lanes
  stopped every indexer at that block. A lane proof a node calls "too deep" halts nothing: of a recent cursor it is that
  node's failure, of one older than the pruning depth a `gap` that is left by itself once a node serves the proof.
  `state`, `caught_up` and `within_lag_tolerance` (the executor's gate) and lean pruning now follow the cursor block's
  verified time; the node's DAA claim is shown, never used. A public node deposed for it that held back chain
  its successor then served is kept away 10 minutes, doubling up to 6 hours; nodes the operator configured are never
  kept away. The small node calls of each step (the sink, the DAG info, a header) now time out after 30 s. New in `/v1/health`:
  `authority_pace` (`keeping_up`, `gaining`, `failing`, `unknown`) and `chain_proofs_off`; `ok`, `/v1/health/ready`,
  `caught_up` and `within_lag_tolerance` are false while the authority is `failing`, and `caught_up` also needs the cursor
  block within 90 s of the host clock. **Changed:** `lag_seconds` in `/v1/health` and `/v1/health/ready` is now only the
  cursor block's age by the host clock (it was at least that, or the node's DAA claim when larger), so a node can no
  longer make the index look behind; it is `null`, and `ready` answers 503 `the cursor block's time is not known yet`,
  until that time is known. `lag_daa` stays, the node's unverified claim. New metrics `kob_indexer_authority_failing`, `kob_indexer_chain_proofs_off` and
  `kob_indexer_cross_check_alone_seconds_total` (also `cross_check_alone_secs_total` in `/v1/health`). On Unix kobd and
  kob take group and other write off the umask at the start, so the log, the data directory and `--state-dir` pass the
  start check whatever the shell's umask.
  **Do:** keep the host clock on NTP (within 30 s); a clock more than 90 s fast makes every node look behind. A network
  that stops making blocks now makes the indexer change nodes every few minutes until blocks come again; that is
  expected (`docs/ops/executor.md`, Part C, *Whether the chain authority keeps up*).
- **Standalone `kobd match` / `kobd keep` take `--state-dir`** (also `KOB_STATE_DIR`) for the in-flight journal and the
  keepers' subsidy window. Without it they keep both in memory only and pay no keeper subsidy. **Do:** add `--state-dir` to
  such units (`kobd --executor` is unchanged: it uses its data directory).

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
- **The public book leaves dust out.** `/v1/books` and `/v1/depth` show no order worth less than half the listing minimum to
  one fill (0.5 KAS by default) unless `include_dust=true` (then each such order or level says `"dust": true`); `dust_orders`
  says how many. An order no single fill can take (its tip above its price, a minimum fill above its maximum fill, a fill
  KIP-9 refuses) is in no book; a decaying ask is valued at its floor. The `/v1/tokens` counts count no dust. Book amounts
  are at most what one fill can take.
- **Charts are whole buckets.** `/v1/candles`, `/v1/quotes` and the pair candles move `from` down and `to` up to a bucket
  start. A lean store's candles may say `"truncated": true` (at most the newest 30,000 fills of the window are read).
- **`/v1/orders?status=`** may return a short (even empty) page with a `next_cursor` when the read in history order reaches
  its budget of 10,000 orders: follow the cursor until it is absent (the web app and `kob` do).
- `kob` refuses a malformed holdings cursor from an indexer instead of looping on it.
- **Every public read has a budget of work** (5,000,000 SQLite steps, 1.5 s of read time per request). A read past it is
  answered `503 read_budget` with `Retry-After: 1`; no honest read comes near it.
- **Books judge an order at the worst end of its window**: a rising bid by its first quote, a curve by its smallest valid fill
  at the mean price a taker pays (its depth is the sequence of its successive smallest fills: an ordinary curve shows every
  level, a bundle forced by its minimum fill shows at its mean price), a pair order by the least amount one fill
  takes at either end. Book amounts and prices are what a fill can take and trade at.
- **No public answer uses the node's claimed DAA score**: `confirmations`, `settled`, an auction's quote now and which orders
  a book takes as fillable come from the store's verified cursor, advanced by the clock, at most 90 s ahead.
- `/v1/pairs/{base}/{quote}/candles` answers `404` for a token no order names. `/v1/depth` says `truncated` and leaves out
  its worst level when two scales' prices round to one level.
- An order view that cannot be rendered answers `404`, never `500`, and such an order is in no book.
- `kob` follows the live-order list to its end (up to 200 pages); a list it cannot finish raises the own-orders guard instead
  of being taken as whole. `kob orders`, `kob balances` and `kob sweep --all` follow their lists the same way and say when
  one is incomplete (see below).

### Behaviour changes of round 8: `kob` and the shared builders

These are new refusals, exits and output that scripts meet. The builders are those of `kob-protocol`, which `kob`, the
web app (through `kob-wasm`), kobd and the tools share.

- **A custody is its order's exact state.** Every builder that spends an order's custody or bought-custody checks its whole
  state against the order's own: token, owner, amount, extension commitment (class) and no borrow setting. A unit of the
  token's covenant id of another class is a stray, never the custody. A cancel-replace's new order must pin the class of
  the tokens it takes.
- **A sweep never takes a custody.** A "stray" whose state is exactly a custody the order still pins is refused; a twin of
  the custody (the same state at another outpoint) is swept only next to the custody it keeps (`SweepOrder.kept`). An
  unbound curve is not swept. `kob sweep` keeps the custody it finds from the order's state.
- `kob cancel`, `sweep`, `replace`, `take` and `recover` take the custody's whole state from the order's own state and
  accept any node UTXO at that script; a view or file naming another custody is not trusted. With none on the node,
  nothing is built.
- **`kob cancel` takes every token UTXO the node shows owned by the order** (the custody's twins included), or refuses
  with "run `kob sweep <id>` first" (strays of another class, or more than one transaction's token inputs): exit 1, where
  it used to cancel and report them "left behind". `--abandon-strays` cancels anyway after naming each UTXO it leaves,
  which is then lost for good; `--yes` alone keeps the refusal.
- `kob replace` (a cancel-replace) refuses while the order owns strays of other tokens or of another class: sweep first.
- **`kob take`** refuses an order whose token program or class is not the market's. When no list names the class, the
  plan says the class is the order's own.
- **`kob recover --cancel`** shows the decoded transaction and fee before it asks, and says that strays the node cannot
  list are lost with the order. `--custody-gone` ends a live order whose custody the node shows nowhere, without it: it
  returns the order's KAS only, says so, and is confirmed at a terminal, never with `--yes`
  (`build_cancel_custody_gone`, kob-wasm `buildCancelCustodyGone`).
- **Text from outside.** `kob` shows tickers, names, server and node errors and the watch feed with control, line
  separator, bidirectional and invisible characters escaped as `\uXXXX`, cut at 160 characters (errors at 400). The
  escape is JSON's own, so `--json` output (and `kob watch --json`) stays valid JSON whose strings parse back to the
  outside bytes exactly. An indexer ticker is used only under the declared-symbol rule (an ASCII letter first, then
  letters and digits, at most 16, not a unit word); otherwise the token is shown by its covenant id, and scripts name it
  by id and give amounts in base units (`1250base`).
- **New refusals** (the first two in `kob`, the last in every caller of the builders):
  - a node that reports less KAS than the indexer's view for an input no signature covers (a custody, a bought-custody, a
    stray) that the view names; the decoded transaction names every such input and the KAS on it;
  - a re-planned `kob replace` of an order at another price scale ("cancel it and place anew");
  - a state with `i64::MIN` in a delivery or curve field (no 8-byte script number spells it).
- **Plans show more.** OCO and trailing-with-take-profit plans show the stop and its worst price and promise the total at
  the worst leg; if-done plans show the committed exit (legs, minimum fill, tip and the repeat's merge cost); `kob replace`
  lists every changed term, old and new.
- **Lists end or say so.** `kob orders`, `kob balances` and `kob sweep --all` follow the indexer's cursor to its end (up to
  200 pages); a list not read to its end is marked `incomplete` (`"incomplete": true` in `--json`), and `kob sweep --all`
  then exits 1.
- **A creator lock counts only the token's class** (that of its first proven genesis output). A lock that holds only another
  class is refused, and with a second class in the genesis the supply is not given (`otherClass`). Shown locked shares can
  fall.
- **Integrators decoding states:** 8-byte state fields (the delivery block, repeat and exit tails, curve numbers) are
  sign-magnitude script numbers, as the covenants read them; a negative zero is refused. No placed state changes.
- `sign_locally` recomputes each digest from the transaction it signs and refuses a stored digest that differs.
- kob-wasm adds `heldCustodies` and `checkReleases`: what a spend that ends an order must release, by exact state.

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
