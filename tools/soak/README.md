# KOB TN10 soak

A live testnet-10 market that looks and behaves like a real one: a test KCC-20 token **TUSD** (program `KCC20Ref_8x8`, fixed supply,
8 decimals) whose KAS price follows **Binance KAS/USDT** (1 TUSD ≈ 1 USD worth of KAS), a second test token **TETH** (same program,
1 TETH ≈ 1 ETH worth of KAS, Binance ETH/USDT over KAS/USDT) with its own KAS book and the TETH/TUSD pair, a market maker quoting both
books, five traders placing every KOB order type in both books plus cross limits and pair market orders between the two tokens, x402 payments (KAS, KCC-20 and swap-and-pay), two competing `kob-executor run` instances (indexer + matcher + keepers), an
invariant checker, and the web UI on live data. Everything runs on one Windows PC against a testnet-10 node of your own
(`ws://127.0.0.1:18210` by default; point `config.json` at it); all KAS comes from self-mining.

Nothing on chain depends on the Binance price: the bots only *read* it to decide where to quote. There is no oracle and no X bot.

## Protocol v3 (no lots): what the soak reads

KOB protocol v3 has no lots. Every quantity (order amounts, `amount_left`, `min_fill`, custody, fills) is token **base units**; every
price and tip is sompi per **whole token**, i.e. per `scale` base units of the order's token (`scale = 10^decimals`, a field of every
order; a cross limit's `price` is token B base units per whole token A). The value of `n` base units at a rate `r` is
`n × r / scale`, rounded in the maker's favour (up for what a maker must receive, down for what a maker pays; docs/spec/order-types.md
"Amounts, prices and rounding"). The indexer views renamed their fields accordingly (docs/ops/executor.md Part B section 5.0); the soak reads:

| the soak reads | v3 field |
|---|---|
| order | `scale`, `min_fill`, `tip`, `budget_rate`, `initial_amount`, `filled_amount`, `amount_left`, `amount_estimated`; `repeat.rpt_amount` / `rearm_amount` / `rpt_price` |
| order `state` (proven) | `scale`, `minFill`, `amountLeft`, `tip`, `maxFill`, `minTouch`, `rptAmount`, `rptPrice`, `rptPre`, `prefund`, `price`, `priceEnd` |
| order `cross` | `scale`, `min_fill`, `price`, `tip`, `price_end`, `worst_price`, `amount_left`, `delivered = ceil(filled × price / scale)` |
| events, `/v1/fills`, WebSocket fills | `amount` (base units) instead of `lots`; `detail.previous.tip`; cross fill `detail.b_due = ceil(n × rate / scale)` |
| trades, candles, stats, depth | `price_basis` = the token's standard scale, `decimals` added |

The checker's invariants, restated in base units with the strength they had in lots (rules.ts header, `quoteOf`):

* **custody** (1): an ask-side order's live custody token UTXO holds exactly `amountLeft` base units (indexer state), and it is in the node's UTXO set; the amount accounting of a live plain ask or KobPair
  is exact: `filled_amount + amount_left == initial_amount`.
* **all-in** (3): a sell receives at least `ceil(n × (q − tip) / scale)`, a buy pays at most `floor(n × (q + tip) / scale)`; a merged repeat take-profit pays the maker
  the proceeds minus `ceil(n × rptPrice / scale)`; a sell-first entry funds its exit with at least `ceil(n × (q − tip) / scale)`.
* **trigger** (5): the evidence fill has the stop's token and `scale`, and its amount is at least `minTouch` base units.
* **repeat** (6): entry `amountLeft` + live booked exit `amountLeft` `<= initial_amount` (base units, to the last unit); an exit's `rptPrice` is the entry's `price + tip` (buy-first)
  or `price − tip` with `rptPre = prefund` (sell-first); the cycle's profit is `ceil(m × (tp − tip) / scale) − ceil(m × rptPrice / scale)`.
* **cross** (11): a fill of `n` base units of A pays the maker at least `ceil(n × rate(t) / scale)` of B (`rate(t) = price − floor((price − priceEnd) × e / auctionDaa)`).
* **agree** (9) compares `filled_amount` and `amount_left` of sampled orders between the two indexers.

A checker state file written under protocol v2.6 (`run/state/checker.json` without `"protocol": 3`) is dropped at start: its memoized terms use the lot fields.
The dated sections below are records of earlier runs under protocol v2.6 and keep their lot wording (and the config keys of that time).

**Bot sizing and prices (v3).** The bots price every book in sompi per whole token (the Binance reference: `price.ts` `RefPrice.perToken`;
the soak tokens have 8 decimals, so the order `scale` 10^8 is one whole token; setup refuses more than 9 decimals) and draw every size as any
amount of token base units, never rounded to a step: the market maker between `mm.minTokens` and `mm.maxTokens` whole tokens of its book,
a trader between half a dollar and its `maxUsd` (TUSD 1:1, an asset token at its USD reference), a pair order for `pairUsd` dollars of
its base token. Minimum fills, trigger thresholds and every KAS amount (bid escrows, prefunds, carriers) come from the web planners
and kob-wasm's exact helpers. The registry the soak writes has no `lot_size` and no `tick` (legacy protocol v2 fields that
`kob_protocol::registry` reads and ignores); the bots and the web app then price on a 1-sompi step per whole token.

## Processes

```
supervisor (scripts/supervisor.mjs)            restarts crashed children (backoff 2 s .. 60 s), rotates logs (20 MB x 5), samples memory / CPU
 ├─ miner      bin/tn10-miner.exe              GPU (OpenCL) or CPU miner, coinbase -> the bank key (maturity 1000 DAA), in bursts below a bank watermark
 ├─ exec-a     kob-executor run                indexer + API :8091, matcher + keepers (hot key execA), x402 facilitator :8401
 ├─ exec-b     kob-executor run                indexer + API :8092, matcher + keepers (hot key execB): competes with exec-a
 ├─ bots       node dist/soak.mjs bots         bank, market maker, 5 traders, x402 merchant (:8480) + payer
 ├─ checker    node dist/soak.mjs checker      protocol invariants -> run/incidents.jsonl, report -> run/reports/
 └─ ui         node scripts/serve-ui.mjs       the web app on live data, http://127.0.0.1:8490/
```

**Why TypeScript for the bots.** The bots drive the web app's own planners (`web/src/kob`: `planOrder` for every order type,
`planCancel` / `planCancelReplace` / `planCancelAll`, the token tracker, the signing pipeline `kob.build -> sign -> kob.finalize ->
kob.validate -> submit`) and the x402 TypeScript SDK (`packages/kob-x402`), with a local Schnorr key standing in for a browser wallet
(`src/wallet.ts`). The soak therefore exercises exactly the code users run, and every transaction passes the script engine before it is
broadcast. Rust stays where it belongs: the executors, and kob-wasm under the planners. `npm run build` bundles `src/` (and the imported
web / SDK sources) into one file, `dist/soak.mjs`, with rolldown from `web/node_modules`; no extra dependencies.

## What the bots do

| Bot | Behaviour |
|---|---|
| bank | consolidates coinbase dust and tops up every wallet (bots and both executor hot keys) in 100 KAS chunks when it falls below its minimum |
| market maker | per book (TUSD/KAS, TETH/KAS and TBTC/KAS, one wallet, one ladder loop each): 6 GTC bid and 6 GTC ask levels around the reference, since 2026-10-03 a near-zero-spread ladder (`mm.innerBps` 2: first level 2 bps from the reference, then every `stepBps` 4 bps; before: 25 / 30 bps), 1 to 6 TUSD each (`mm.minTokens` / `maxTokens`, any base-unit amount in between; per-book overrides: 0.0004 to 0.003 TETH, 0.000012 to 0.000072 TBTC), skewed by the inventory it gained since start (at most `maxSkewBps` 8); a level is **amended** (cancel-replace in one tx) once it drifts by its own distance from the mid (at least `requoteMinBps` 3, at most `requoteBps` 20; mm-math.ts), the side the reference moves away from first; surplus orders are cancelled, a 2 % reference move or every 30 min resets the ladder with **cancel-all** |
| traders t1..t5 | one action every 45 to 120 s (exponential), weighted over every order type of `docs/spec/order-types.md`: market (auction), limit GTC (passive and crossing), GTD (10 to 40 min: keeper refunds), day, timed activation, IOC, FOK, streaming, stop-market, stop-limit, trailing stop, take-profit, OCO, IFD buy-first / sell-first, IFO, IFO with stop entry, repeat IFD, repeat IFO, TWAP, DCA, Dutch / rising bid, close, cancel, cancel-replace (amend), cancel-all |
| pair orders | a share of the trader actions (`token2.pairShare`, older spelling `crossShare`, 15 %) are **pair orders** of the asset token A (TETH, TBTC) against TUSD B through the web planner (`planOrder` with a PairPlanEnv built by `market.ts` `pairPlanEnv`, the same environment as the app's `buildPairPlanEnv`): KobPair limits on both sides (35 % resting 0.2 .. 1 % passive for 5 to 20 min, the rest crossing by 0.2 .. 1 %: the matcher nets opposite pair orders or routes them through the two KAS books), IOC and market orders, Dutch, TWAP / DCA, FOK, streaming and close, KobCondPair stop-market, stop-limit, trailing stops, take-profit and OCO (armed by the matcher from pair evidence: two KAS-book fills of A and B, or a resting pair order filled at or beyond the stop), KobIfdPair IFD / IFO buy-first and sell-first, IFO stop entries and repeat IFD / IFO: every order type of the KAS ticket (`DEFAULT_PAIR_WEIGHTS` in traders.ts, `token2.pairWeights` overrides). Prices are B base units per whole A around the pair book's touch midpoint (when within 3 % of the Binance-derived fair rate refA / refB, else the fair rate); sizes 3 to 20 USD of A (`pairUsd`). Pair fills are volume only: they never set a price (checker invariant 11). The retired cross limits (`KobCross`) and their bot paths are gone |
| x402 | a paywall with three resources (`/native` 0.5 KAS, `/token` 0.25 TUSD, `/swap` 0.5 KAS paid with TUSD through KOB bids) settled by exec-a's facilitator; the payer pays them in turn every ~2 min. With `x402.invoiceIntent.enabled` every fourth turn is an **invoice paid by an intent** (docs/ops/executor.md A.7): the merchant registers an invoice of `amountKas` (20) KAS whose only entry is an intent swap offer paid with TUSD, the payer signs the router intent's creation (`payInvoiceWithIntent`, at most `maxSellTusd` sold) and the facilitator executes it against the TUSD bids; the supervisor then switches the facilitator's `intents` (keeper = exec-a's key) and `invoices` on |

**Token consolidation** (`consolidate` in the config; the market maker per book, the traders too). Every fill, refund and fan-out leaves a token UTXO with a ~10 KAS carrier, so a bot wallet fragments. After each ladder pass (each trader action) a bot counts its plain, unreserved token UTXOs per token; above the fan-out target (`token2.fanout`: 8 for the market maker, 4 for a trader; TUSD 8 / 4) plus `slack` (8) it merges the **smallest** ones, up to the program's token inputs (8 for `KCC20Ref_8x8`) into one UTXO per `sendTokens` transaction, fee and change from the freed carriers (no KAS UTXO is spent), never taking the visible count below the fan-out target (so the fan-out is not undone and parallel sell-side placements keep their inputs). At most `maxTxPerInterval` (3) merge transactions per `intervalSec` (60) per key and token; counters `tx_ok:consolidate`, `consolidate_merged:<ticker>`, `consolidate_freed_sompi`, gauge `token_utxos:<key>:<ticker>` in `run/stats/bots.json`. `"consolidate": {"enabled": false}` switches it off, `"traders": false` leaves the traders out.

Sizes are half a dollar to the trader's `maxUsd` (3 to 8 USD; 1 TUSD is about 22 KAS), in every book (an asset token at its USD reference), any amount of base units.
`token2.bookShare` (30 %) of the other trader actions trade the TETH book, the rest the TUSD book; their stat tags carry `@TETH`. Every plan refusal is counted by code (`plan_refused:<action>:<code>` in
`run/stats/bots.json`), never retried blindly.

## Layout

```
tools/soak/
  config.example.json   template of run/config.json
  src/                  bots, bank, price feed, setup, checker (TypeScript)
  scripts/              keygen.mjs, supervisor.mjs, serve-ui.mjs, start.ps1, stop.ps1, status.ps1
  test/                 unit tests of the checker's pure logic (npm test)
  screenshots/          UI screenshots on live TN10 data
  run/                  (gitignored) keys, state, databases, logs, stats, reports
```

`run/` holds every secret (`keys.json`, `exec-*/operator.key`, `x402-merchant.key`) and is gitignored.

## Build (once)

From the repository root (Git Bash), `CARGO_BUILD_JOBS=4` on a 16 GB machine:

```
cargo build --release --features gpu --manifest-path tools/wallet-gate/miner/Cargo.toml --target-dir target/miner   # GPU + CPU backends
cargo build --release --locked -p kob-executor --features deploy-tn10     # the TN10 deployment build
scripts/build-wasm.sh --tn10 --web                                         # kob-wasm node + web bindings of the TN10 build (pkg-node-tn10, pkg-tn10)
cd web && npm ci && npm run fetch-sdk && cd ..
cd tools/soak && npm run build && npm run typecheck && npm test     # typecheck reads web/wasm: run `npm run build:wasm` in web/ once
```

Then copy the binaries and bindings into `run/` (the soak runs from copies, so later rebuilds never touch a running process):
`run/bin/{kob-executor,tn10-miner}.exe`, `run/wasm-node/` (= `crates/kob-wasm/pkg-node-tn10` plus a `package.json` saying
`{"type":"commonjs"}`), and the UI: `tools/soak/scripts/build-ui.sh` (a `vite build` of `web/` against `crates/kob-wasm/pkg-tn10`, into
`run/web-dist`, without touching `web/wasm`).

To replace a binary while the soak runs: `scripts/stop.ps1`, copy, `scripts/start.ps1` (the databases resume from their cursors).

## First start

```
node scripts/keygen.mjs                              # run/keys.json, executor key files, x402 merchant key
cp config.example.json run/config.json
node dist/soak.mjs setup --config run/config.json    # TUSD (and token2) issuance, run/registry/tokens.json
powershell -File scripts/start.ps1
```

`setup` is idempotent (state in `run/state.json`). Stops need no setup: since protocol v2.6 (touch trigger) a stop arms or trails in the
transaction that fills a resting ask / bid of the same token (see the checker's invariant 5); there are no receipts to create.

## Redeploy on a new protocol version (same keys, same TUSD)

The token program does not change with the order covenants, so a new run keeps the funded keys and the TUSD token: copy `keys.json`,
`exec-*/operator.key`, `x402-merchant.key` / `.sha256` and the `token` / `genesisOutputs` of `state.json` into the new `run/`;
`setup` then only writes the registry. The executors start at the sink (`--start sink`), so their indexers never saw the holdings
created before: seed `run/state/tracker-<key>.json` with each key's token UTXOs (the bots' token tracker verifies every candidate on
the node). The previous run is wound down first with its own code (branch `m5/soak`: `soak.mjs winddown balances|cancel|strays|retire|consolidate`):
cancel every order, retire the receipts, and merge each key's token UTXOs into one (it also frees the KAS carriers of the deliveries).

## Operate

| | |
|---|---|
| status | `powershell -File scripts/status.ps1` (children, restarts, memory, indexer health, the latest report) |
| stop | `powershell -File scripts/stop.ps1` (creates `run/STOP`; the supervisor stops every child) |
| start / restart | `powershell -File scripts/start.ps1` (state, databases and the record logs persist; executors resume from their cursor) |
| one child only | `powershell -File scripts/start.ps1 -Only miner,exec-a,exec-b` |
| restart one child | `powershell -File scripts/restart-child.ps1 -Name miner` (run/RESTART-<name>: the supervisor restarts only that child, its spec rebuilt from a fresh read of config.json; the others keep running) |
| swap the miner binary | rename `run/bin/tn10-miner.exe` (a running exe can be renamed, not overwritten), copy the new one in, `restart-child.ps1 -Name miner` |
| miner watermarks | edit `run/miner.json`: the running miner re-reads it on change (see "Miner" below); `config.json` `miner` is the persistent value |
| pause a matcher | create `run/exec-a/pause` (delete to resume) |
| logs | `run/logs/<child>.log` (+ `.1` .. `.5`), `run/logs/supervisor.log` |
| report | `run/reports/latest.txt` / `.json` (hourly copies alongside), or `node dist/soak.mjs report --config run/config.json` |
| incidents | `run/incidents.jsonl` (one line per invariant violation) |
| bot counters | `run/stats/bots.json`; every bot transaction with fee and masses: `run/state/txs.jsonl`; x402 payments: `run/state/x402-payments.jsonl` |
| resources | `run/stats/resources.json` (latest), `run/stats/resources.jsonl` (one sample a minute) |
| UI | http://127.0.0.1:8490/#/market/<token covenant id> (one market page with BASE / QUOTE selectors: TETH/KAS, TBTC/KAS, KAS/TUSD for the USD reference token, flip toggle), the pair `#/pair/<TETH id>/<TUSD id>` (read-only; connect a TN10 wallet to trade). The build pins the soak registry (`scripts/build-ui.sh`, `UI_REGISTRY` to override): rebuild it after `soak.mjs setup` rewrites the registry |
| cancel an order | `node dist/soak.mjs cancel --config run/config.json <covenant id> ...` (the maker's key, the web app's cancel path) |
| balances | `node dist/soak.mjs balances --config run/config.json` (JSON: per key the node's KAS total, spendable KAS, and per token the proven holdings: UTXO count, amount, KAS locked in carriers; read-only) |
| merge every holding | `node dist/soak.mjs consolidate --config run/config.json [<key> ...]` (redeploy helper, bots stopped: each key's plain token UTXOs merged into ONE per token in rounds of up to 8 inputs, fee and change from the freed carriers; the merged output carries 10 KAS, or 1 KAS when the inputs carried less; the outputs are created after the new indexer's start, so it lists them) |
| add a second token | add `token2` to run/config.json (see config.example.json), `scripts/stop.ps1`, `soak.mjs setup` (issues it, rewrites the registry = the executors' allowlist), `scripts/start.ps1`; an interrupted setup whose issuance was broadcast: `setup --recover-issue token2:<txid>` |
| holdings | `node scripts/holdings.cjs [--seed]` (every key's token UTXOs from the indexer and the trackers, verified on the node; `--seed` writes them into the trackers: a fresh indexer never lists pre-run holdings) |
| bot fees | `node scripts/fee-compare.mjs --before <ISO> <ISO> --after <ISO> [<ISO>]` (fees, sizes, payloads per action between two windows of `run/state/txs.jsonl`) |
| screenshots | `node scripts/screenshots.mjs` (market list; every token's market page dark and flipped, the first also light / 1280 px / phone; the pair pages with TUSD; My orders and an unsigned confirmation of trader t1 through the web app's test wallet set to refuse signing; into `screenshots/`; `--only <name part>`, `--url <public URL> --tag public`, `--no-wallet`) |
| activity | `node scripts/activity.mjs [--minutes 10] [--json]` (per KAS book fills and trades per minute, the market page's recent trades; per pair the pair fills per minute by counterparty and their `price_source`; read-only) |
| tracking | `node scripts/tracking.mjs [--minutes 60 \| --from <ISO> --to <ISO>] [--json]` (KAS/USD, ETH/USD, BTC/USD from the indexer's trades and 1m candles, converted as the app does, against the bots' Binance record `run/state/ref-prices.jsonl`: median / p95 / max deviation, bias, lag) |
| indexer API | http://127.0.0.1:8091/v1/health, `/v1/stats/<token>`, `/v1/candles/<token>?interval=5m`, `/v1/trades/<token>`, `/v1/depth/<token>` |

## Miner

`tools/wallet-gate/miner` (README there): CPU threads or the OpenCL kHeavyHash kernel (`--features gpu`), every nonce re-verified with
`kaspa_pow` before `submitBlock`. The supervisor turns `config.json` `miner` into `run/miner.json` (validated by
`scripts/miner-config.mjs`; a bad block stops the supervisor before anything starts) and passes it as `MINER_CONFIG`:

```
"miner": { "backend": "auto", "lowKas": 20000, "highKas": 30000, "maxDuty": 1, "pollSec": 15,
           "gpu": { "device": "NVIDIA" } }
```

* `backend`: `gpu`, `cpu` or `auto` (the GPU if one opens, else the CPU; also the default of a GPU build). `threads` (or the older
  top-level `minerThreads`) only matter for the CPU backend.
* **Intermittent mining**: with `lowKas` / `highKas` the miner mines only in bursts: one starts when the bank's spendable KAS (node
  UTXO index, coinbase counted once 1000 DAA old) falls below `lowKas` and ends at `highKas`; otherwise it idles (no templates, no CPU,
  no GPU). Without them it mines continuously, as before. `maxDuty` (0..1] caps the mining time over `dutyWindowSec` (600).
* The balance is read in the background on its own connection: a full UTXO listing of the bank (78k coinbase UTXOs, a 30 MB answer,
  ~30 s from this PC) only while mining and for 200 s after, else the cheap `getBalanceByAddress` total.
* Edit `run/miner.json` to move the watermarks of the running miner (re-read on change); the next supervisor start rewrites it from
  `config.json`. Log: `run/logs/miner.log` (`mining: burst ...` / `idle: ...` on each state change, hashrate every 30 s while mining).
* A miner started by an older supervisor (no `MINER_CONFIG`) reads `tn10-miner.json` next to its executable instead.

Measured on this PC (RTX 3060 Laptop, driver 555.97): GPU ~92 MH/s at 0.01 CPU core (work-group 128, 60 ms dispatches; the GPU ran
at 900 MHz SM clock, power-limited at ~37 W); the CPU miner did 2.4-2.7 MH/s on 12 threads at ~6 cores.

## Notes and limits

* **Reference price.** Binance has no KAS spot market; the only Binance KAS/USDT market is the USD-M perpetual, read from the public
  REST endpoint `fapi.binance.com/fapi/v1/ticker/price?symbol=KASUSDT` every 3 s (no key). A stale price (> 2 min) pauses the bots.
  The second token's reference is `ETHUSDT` from the same endpoint over `KASUSDT` (a lot of 0.0001 TETH ≈ 6 KAS): an **independent real
  feed**, not a synthetic ratio to TUSD, so the TETH/TUSD pair price moves the way two real assets do (ETH and KAS move independently).
  Resting cross limits then drift in and out of the money and the route's implied quotes move, which is what the pair book, the cross
  planner and the matcher's route ranking have to handle; a fixed ratio would freeze the pair price (a cross limit either always or never
  fills) and exercise none of it.
* **Checker, second token.** Every invariant runs over every soak token (orders, fills read across tokens, custody of pair orders: each custody
  of its state, of A and / or B, strays, indexer agreement), plus two more: **11 pair** (since the pair phase, 2026-10-06; before: 11 cross,
  the retired cross limits), every pair order fill (KobPair, KobCondPair, KobIfdPair) carries NO price (`price` null, `detail.pair.price_source`
  none: a pair fill is volume only and never sets a price, candle or last price) and honours the order's guarantee at its quote q
  (`detail.pair.price`, B base units per whole A): an ask receives at least `ceil(n × q / scale(A))` of B, a bid pays at most
  `floor(n × q / scale(A))`; every arm / trail of a pair stop names evidence of one of the two modes (two KAS-book fills of A and B, or a resting
  KobPair filled); **12 supply**, per token the live token UTXOs (the indexer's plus the genesis / pre-run holdings it never saw, counted while
  they are in the node's UTXO set) add up to the supply, persisting 3 rounds. The report shows one market line per token, the transactions that
  filled several books and pair orders (with the counterparty split route / netting / inventory and the trigger modes), and the supply deltas.
* **KRON test token: skipped.** KOB's issuance (`kob-protocol::issue`, `kob token issue`, the web issuance) builds KCC-20 only; a KRON token
  is minted by KRON's own tooling through its minter program, which has no TN10 deployment. The KRON family stays covered by the executor's KRON twin tests.
* **Stops arm on a touch.** A conditional order or stop entry arms (or trails) only in a transaction that fills a plain resting ask (bid) of the
  same token that quoted at or beyond the stop, trades at least `minTouch` base units (v2.6: `minTouchUnits`) and was exposed for `minRestDaa` DAA (default 50). The checker
  (invariant 5) asserts every `arm` / `trail` event is backed by such a fill in the same transaction; a candidate whose exposure the indexer
  data cannot prove is a `warn`, no candidate is an error. The `R_ID` deployment profile is no longer needed for stops.

## Second token TETH and cross limits (2026-10-01, TN10, protocol v2.6, main fd16f60)

The if-done exit fix of main fd16f60 went live first (both executors restarted together; each logged one "backfilled the extension
commitment of if-done exits" line, 11 exits): the two exits the UI could not cancel before (`254adb2e…`, `ec5bb096…`) were cancelled through
the web app's cancel path (`soak.mjs cancel`, order view -> `snapshotFromOrderView` -> `planCancel`, engine-validated) and are `cancelled`.

**TETH** (`Test ETH (tETH)`, covenant `827f13e9…`, KCC20Ref_8x8, 1,000 supply, 8 decimals, lot 0.0001 TETH ≈ 6 KAS, tick 10,000 sompi per lot)
was issued by `setup` with KOB's own issuance (kob-wasm `issue`, tx `0817eb30…`): 30 % to the market maker, 8 % to each trader, the rest to
the bank. Setup broadcast it and then failed on its own lot parser before recording it; `setup --recover-issue` (added) rebuilt the record
from the node, and setup now validates the configuration before it broadcasts.

Watch window 00:09:30 to 00:58:30 UTC (49 min; one restart at 00:24 for an executor log line), from the indexer:

| | |
|---|---|
| market | 416 fills in 124 transactions: TUSD 214 fills / 89 txs, TETH 202 / 83; the largest transaction 9 fills |
| several books in one tx | **48** transactions filled both the TUSD and the TETH book, every one of them with a cross fill; 0 with two cross fills (see the budget finding); 0 plain two-book batches |
| cross limits | 82 placed (53 GTC / GTD, 25 IOC pair market, 4 FOK) in both directions; **48 cross fills** (TUSD→TETH 25, TETH→TUSD 23), every delivery exactly `n × (bLot − tipLot)`, proven by the indexer and (47 of 47 checked) found on the node |
| stops | 5 armed, 1 triggered fill with evidence, 0 trails |
| x402 | 28 settlements (native 11, KCC-20 9, swap-and-pay 8), 0 failed, 0 ambiguous |
| incidents | 0 (all invariants incl. 11 cross and 12 supply: TUSD and TETH supply deltas 0 every round) |
| executors | 0 rejected transactions; matcher profit 153 KAS on 45 cross transactions (avg 3.4 KAS) against 0.09 KAS avg on a plain batch |
| resources | all processes 474 MB (executors 38-47 MB) |

Findings (evidence in the logs of this run):

* **Compute budget table short for two cross limits in one route (kob-protocol).** A batch with two cross limits selling TETH (one closing,
  one resting), 4 TUSD ask custodies, 2 TETH bids (and an arm update) needs 301,313 to 304,453 script units on the closing cross input
  at the table's `KobCross.settle.close@KCC20Ref_8x8+KCC20Ref_8x8` = 29 units (limit 299,999), and 310,045 with the one-unit safety net
  (limit 309,999), so the matcher skips the batch on every tick while that pair of crosses stays eligible
  (`skipped why=…ExceededCommittedScriptUnits { used: 310045, limit: 309999 }`; 72 failed ticks on the two executors in two windows of
  about 30 s). The generator (`tests/budget_table.rs`) measures one cross limit at the widest route and two cross limits only against
  1 bid + 1 ask. Result: no transaction with two cross limits went through in the window.
* **Pair market orders always fill at their worst price.** A cross limit is paid exactly `n × (bLot − tipLot)`; it has no auction, so
  matcher competition never improves it (in the KAS books an auction does). The IOC pair market order is a cross limit at the worst route
  price after slippage, so it fills at exactly that bound and the matcher keeps the rest (153 KAS on 45 cross transactions here, about 2 %
  of the routed value at the soak's 6 % slippage). The ticket text "fills at {worst} at worst" suggests it can be better; it never is.
  **Fixed in protocol v2.6: pair market orders are auctions** (the guaranteed rate starts at the route's best level and relaxes to the
  worst bound over 20 s, so matcher competition improves what the maker gets; the checker's invariant 11 uses the rate at the fill's time
  `detail.t`). The tip of a cross limit is KAS (prefunded in the order, released per filled lot) and the maker receives AT LEAST `n × rate(t)` of B,
  the route's sub-lot remainder included (invariant 11 errors only on less; the surplus is reported as `crossSurplusFills` / `crossSurplusUnits`).
* **Route remainder.** The route buys the received token in whole ask lots and the matcher keeps the remainder of the last lot as a cost
  (matcher.md 3.5). Cross limits sized naively (random amount) waste 4.6 % of the delivery on average and mostly do not fill (pair market
  0 of 2, marketable limits 6 of 8); sized so that the delivery is almost whole lots of the received token (1.1 % waste), 18 of 21 pair
  market orders and 20 of 23 marketable limits filled. Neither the pair book's route levels nor the ticket account for it.
* **Operator token dust.** Every route remainder is a new token UTXO of the operator with a 10 KAS carrier, never merged or sold: 51 minutes
  after the second token went live exec-a held 22 such UTXOs (220 KAS of carriers for 0.33 TUSD + 0.00018 TETH), exec-b 28 (280 KAS for
  1.77 TUSD + 0.00029 TETH), against about 200 KAS of spendable funding each: the bank's top-ups now flow into token dust.
* **Lowering short of funds.** 10 batches in 30 minutes were skipped with `lowering: insufficient funds: need N, have N - (1..9 KAS)`;
  the lowering is handed one funding UTXO (`engine.rs` `self.pool.first()`). Not root-caused.
* **Web: a resting cross limit blocks every buy of its token A.** `ownOrderRefs` (web/src/app/env.ts) lists a cross limit (no KAS price)
  as a sell at price 0, so `checkSelfTrade` refuses every buy of that token with SELF_TRADE while the cross limit rests (reproduced with the
  live order view). The bots leave cross limits out of their own-order refs (`market.ts planEnv`).
* **Web: crossed pair book.** Two resting cross limits on opposite sides never fill each other (no cross-to-cross matching), so the pair
  book can stay crossed indefinitely; the page says "Crossed by …: matchers are catching up", which is not what happens.
* **Executor: skip reasons were not logged** (fixed on this branch, `matcher: log why a book or job was skipped`): the budget finding
  above was invisible before.
* A crossed sell-first IFD entry in the TETH book (4 lots, 0.03 KAS per lot under the best bid) stayed unfilled for 4 min with no skip
  logged until its maker cancelled it; the small TETH lot (≈ 6 KAS) leaves little spread per fill. Not root-caused.

Soak-tool findings fixed on `m9/soak`: setup broadcast before validating its lot size (now validated first, `--recover-issue`); the bots
built a new wallet object (own input reservations) per use of one key (the market maker's two books, the fan-out): one wallet per key now;
the checker could not check a cross limit filled between two rounds (an IOC pair market order lives seconds): its terms now come from the
order view's `cross` object.

Fixed on `m10/fix` (supersedes the findings above where noted; the pricing of cross limits, pair market orders included, is unchanged
pending a decision):

* **Budget table** (kob-protocol): the generator now measures every multi-cross route the planner can build (no and the most other
  resting cross limits on the token-A outputs, the most sold-out ones, the most asks of B, with and without plain bids of B, at 1 and
  200,000 lots, every program pair and branch); `KobCross.settle.close@KCC20Ref_8x8+KCC20Ref_8x8` 29 -> 39, `.rest` 37 -> 44, `.ioc`
  35 -> 41 (193 roles changed). A cross limit input's cost is set by its route only: about 16,400 units per bid delivery, 49,800 per
  other cross limit's rest, 18,900 per ask of B. Should the engine still reject a budget, the matcher measures the inputs and retries
  with what they use instead of skipping the batch tick after tick.
* **Operator token dust**: the executor's maintenance jobs (`docs/ops/executor.md`, Maintenance) merge the operator's token UTXOs of a
  token (the freed carriers back to the funding) or sell whole lots into a bid the tick left when that pays the fee; on by default,
  2 per tick.
* **Lowering short of funds**: root cause, the lowering was handed the largest funding UTXO only (`self.pool.first()`) while the
  shortfall (1 to 9 KAS) was within the pool's total. A batch now takes as many funding UTXOs as the builder's shortfall needs (8 at
  most, deterministic) and merges up to two of the smallest into its change while the pool holds more than four.
* **Crossed sell-first IFD entry** (`b1603a65…`, created 00:54:01, cancelled 00:58:19): not crossed. It was an `ifoStopEntry`, a
  sell-STOP entry: limit 6.0674 KAS per lot, `entryStop` 6.1042, `minRestDaa` 50, unarmed. A stop entry fills only next to its trigger
  evidence: a plain resting ask of TETH filled at or below 6.1042 in the same transaction, exposed 50 DAA before the lock time.
  In its 4 minutes the only plain ask of TETH that traded at or below 6.1042 was the IOC ask `e27c7b37…` at 6.1022 (created DAA
  584848248, filled with lock time 584848293: 45 DAA of exposure, 5 short of 50), so nothing could fill it: correct. What made it look
  crossed: the indexer listed every if-done entry in its KAS book at its limit, unarmed stop entries too. An unarmed stop entry is now
  in no book (books, depth, counts) until it is armed; the start-up migration corrects an existing database.
* **Web: a resting cross limit blocked every buy of its token**: the self-trade guard now prices an own cross limit at its implied
  quote (from the best plain ask of B as a sell of A, from the best plain bid of A as a buy of B); one that cannot fill is left out. The
  bots' workaround (`market.ts planEnv` leaves cross limits out of the own-order refs) stays: removing it is not covered by the soak's
  tests, and with the fix it changes nothing.
* **Web: crossed pair book**: an overlap made only of resting cross limits on both sides is shown as such ("Resting cross limits
  overlap ... do not fill each other directly"), not as matchers catching up.

## Second soak run (2026-10-01, TN10, protocol v2.6)

Redeployed on main 5e4c00e (touch triggers, global batch planner, arming by default) from `KOB-wt/m9-soak`, reusing the keys and TUSD
of the first run. First hour (22:09 to 23:05 UTC, one full restart at 22:43): 211 fills / 98 trades, 164 TUSD = 3,772 KAS traded,
451 orders of every kind; executors 0 rejected transactions, every lost race benign (`MissingInput` / `DoubleSpend`); 8 stops armed or
triggered by same-transaction evidence (7 checked against the touch rule by an independent audit, all consistent; one stop closed before
its terms were captured); x402 22 settlements (native 8, KCC-20 7,
swap-and-pay 7), 0 failed, 0 ambiguous; all processes together about 400 MB.

Soak-tool findings fixed on `m9/soak` (with tests): the x402 payer set no spend ceiling (`maxAmount`), which the SDK now requires
(`spend_not_authorized` on every payment); `build-ui.sh` did not stage `web/csp.mjs` / `registry-pin.mjs`; the checker read the terms
of an evidence order it never saw live without its token (2 false `trigger` errors), skipped arms of stops that filled before its next
round, and did not check triggered fills; it now sweeps the newest orders every 5 s for their full terms, checks triggered fills like
arms and bounds an unknown exposure from below. Open: a booked sell-side exit's view has no `extension_commitment` and its custody no
state, so `snapshotFromOrderView` cannot cancel it (indexer: exits are created with `ext_commit` null).

## First soak run (2026-09-29, TN10)

This run used protocol v2.5 (trade receipts, `R_ID` deployment build); v2.6 replaced them by the touch trigger above, so the receipt findings below are history.

About 3 h of continuous running on the TN10 deployment build (supervisor uptime 03:03 at 19:59 UTC; executors unchanged for the last
2 h 50 min), 16 GB Windows laptop:

| | |
|---|---|
| market | 658 fills / 335 trades, 538 TUSD = 12,080 KAS traded, TUSD 21.75 .. 22.84 KAS following Binance; 2,000 orders of every kind (KobAsk 814, KobBid 740, KobCondAsk 174, KobIfdAsk 120, KobCondBid 107, KobIfdBid 45) |
| executors | exec-a 100 / exec-b 135 finalized transactions (3.68 / 6.43 KAS matcher income), 290 lost races between them (mempool double spends, benign), 0 rejected; keepers: refunds, IOC / FOK kills, a stop armed by a matcher-minted receipt |
| x402 | 166 settlements accepted (61 native, 57 KCC-20, 48 swap-and-pay), 1 failed (a soak quote bug, fixed), 0 ambiguous |
| checker | 0 open incidents; 1 false positive fixed in the checker (a rising bid's surplus returned on the delivery) and 1 explained (a dust change folded into the fee by design) |
| resources | all soak processes together ~400 MB working set (executors 30-50 MB each); the miner takes 12 threads |

Findings fixed on the branch (each with a regression test): the committed reference templates can never arm a stop on a real network
(`R_ID` placeholder: the `deploy-tn10` profile); the market page crashed on a momentarily crossed book (negative spread in `roundDiv`);
the keeper armed with the KCC-20 receipt id for every family; a thin, competitive book never paid for a receipt mint, so stops rarely armed
(`mint_keeper_credit`); bid fill events now expose what a buy cost (`escrow_before`, `escrow_after`, `delivery_value`). Web app findings since fixed (with tests): the re-org banner auto-dismisses after 6 s; a transiently crossed book is never drawn (the last uncrossed snapshot is held, then a "matching in progress" placeholder); bids whose escrow no longer funds one lot are dropped from the book and depth (they stay in My orders with a refund prompt); an amend can add KAS to a partly filled bid (cancel and a larger replacement in one transaction). Nothing open from this run.

## Redeploy on main 84d8efd (2026-10-01, TN10, protocol v2.6): cross limit with pair-market auctions

Rolled into the running soak (same keys, same tokens, same databases) from f7eadfc: the old `KobCross` template (state 16 bytes shorter: no
`bLotEnd` / `auctionDaa`; the tip was in token B) cannot be decoded by the new build, so the live cross limits were wound down first with
the OLD binaries: the bots' `token2.crossShare` set to 0 (bots restarted), every live cross limit of the bots' keys cancelled with
`soak.mjs cancel` (17 GTC / GTD, 2 partly filled; the IOC ones had been refunded by the keepers), zero open at the end, zero strays.
Then `merge --ff-only main`, executor `--features deploy-tn10`, `build-wasm.sh --tn10 --web`, `npm run build`, `build-ui.sh`, stop, copy
binaries and `wasm-node`, start, `crossShare` back to 0.2. The databases were NOT rebuilt: the new build opens them and resumes from the
cursor with no migration line (the old cross orders stay readable as `KobCross` rows of the old template hash).

Watch window 07:57 to 08:43 UTC (46 min, no restart, 0 incidents beyond one expected `trigger` warn at the swap), from the indexer:

| | |
|---|---|
| operator token dust (maintenance) | exec-a 217 token UTXOs / 2,170 KAS locked -> 2 / 20 KAS, exec-b 258 / 2,580 -> 2 / 20 (about 10 min after the start: 28 + 48 merges, 38 + 46 sells in the logs); free KAS 327 -> 2,891 and 177 -> 3,207 |
| skips | 0 `ExceededCommittedScriptUnits`, 0 `insufficient funds`, 0 `skipped why` on both executors (before: 72 failed ticks in 30 min and 10 lowering shortfalls in 30 min) |
| cross | 84 orders (54 GTC / GTD, 23 IOC pair market, 7 FOK), 51 fills (19 auction, 32 limit), 47 transactions with a cross fill, **4 with two cross fills**; every delivery at least `n x rate(t)` (checker invariant 11: 553/553, 0 below due, 47 with a route remainder delivered to the maker) |
| pair market auctions | the 19 fills landed at a rate 0..1 of the way from start to worst: 2 in the first decile, 8 of 19 in the last, 4 exactly at worst; median 0.77, mean 0.67; 0.7 s to 21 s after the activation (median 15.5 s); 2 deliveries beat the start rate (remainder) |
| matcher income per cross transaction | avg 2.86 KAS (single 2.78 on 44, two-plus 3.81 on 4) against 3.94 in the 6.7 h before (single 3.84, two-plus 5.37 on 28; the 3.4 of the first hour); a plain batch 0.07 vs 0.10 |
| executors | 0 rejected transactions, every lost race benign; both indexers following, lag 0 to 8 DAA |
| resources | all processes 563 MB (executors 37 to 47 MB) |

Findings (evidence in this run's logs):

* **An old record log cannot be replayed by the new build.** `kob-executor index replay` of the pre-swap exec-a record log (8,388 frames,
  copied before the first start) fails with `record log frame 1356 is corrupt: malformed record: truncated` on the new binary and replays
  all 8,388 frames on the old one: the wire format stores a reveal's state at the template's CURRENT `state_len`, and the first spend of an
  old `KobCross` reveals 16 bytes less. The running databases are fine (they resume), but the documented resync path (README, executor.md
  7.6) is broken for any log that holds an old cross; the new frames written since are in the new layout, so the log is now unreplayable by
  either build as a whole. The pre-swap databases and binary are in `run/backup-pre-958d013/`.
* **The maintenance and matcher act while the indexer is still catching up.** In the first seconds after a start both executors submitted
  maintenance merges / sells while the log said `the book lags the node; planning nothing` and `operator funds are low funding=0`
  (2 and 18 lines, all within 35 s of the start); two sells lost a race with `MissingInput` on stale state. Harmless here.
* **The pair page still shows a crossed book for a long time** ("Crossed by 233.61 TUSD: matchers are catching up"): resting cross limits
  on opposite sides never fill each other (known, unchanged).
* The market maker's wallet fragments: 687 TUSD and 491 TETH UTXOs (about 15,000 KAS of carriers) after 1.5 h; not an executor matter. Addressed bot-side: the bots now merge their surplus token UTXOs (config `consolidate.enabled` / `slack` / `maxTxPerInterval` / `intervalSec` / `traders`, see "What the bots do").

`soak.mjs balances` (added) prints the per-key balances used for the before / after record of the wind-down.

Fixed on `m11/fix` (not yet rolled into the soak):

* **Record log** (kob-executor): frames are versioned. Format 2 (written from now on) starts with `0x00 0x02`, carries a template table
  (record-log code -> template hash of every order template and token program the frame refers to) and length-prefixes every revealed
  state, so a frame decodes whatever the templates are when it is read. Format 1 (all frames written so far) is read with today's layouts
  plus every retired layout the repository ever committed (`indexer/layouts.rs`, 57 rows; chosen by the entry's dispatch tag, the whole
  frame deciding when two fit). A reveal of a layout this build does not have is dropped and counted; a frame it cannot decode at all is
  kept and skipped with a warning instead of aborting. The pre-958d013 exec-a log (copy) now replays end to end: 8,388 frames, 864 old
  `KobCross` reveals (333-byte state) dropped, its 777 old cross limits become placement rejects; every other order, event and holding
  matches the database the old build kept (`tests/recovery.rs`, `an_old_record_log_replays_end_to_end`, ignored, `KOB_OLD_RECORD_LOG`).
* **Matcher before the indexer caught up**: the runner plans nothing (matcher, keepers, maintenance) until the follower reports `following`
  (`caught_up` in `/v1/health`); the restarts of 07:21 had planned over a store 213 DAA behind while the follower's first batch took 24 s.
  `operator funds are low funding=0` is no longer logged for a step that read no funding.
* **Pair page "catching up"**: a crossing is a backlog only when the route can pay a lot of the cross limit in whole lots of the token it
  receives (the planner's rule); the live overlap (cross asks of 6 and 8 TETH lots at 2,561 / 2,579 against a route bid of 2,673: 0.15 /
  0.21 TUSD of a 1 TUSD lot) now says so instead of "matchers are catching up".
* **Late pair-market fills**: root cause is whole-lot delivery, not the planner, fees, ticks or competition. A matcher buys the received
  token in whole ask lots and delivers all of it, so a fill of `n` lots at `rate(t)` costs `ceil(n x rate(t) / lot)` lots. The bots sized
  `n` so that `n x worst` was just under whole lots: at every better rate the order asked for one lot more than the route pays (6.2 KAS for a
  TETH lot, 23.3 KAS for a TUSD lot, against matcher margins of 0.16 to 10.3 KAS). 28 of 30 single-cross fills (07:56 to 09:15 UTC) landed
  0 to 19 DAA after the auction crossed that step, and 31 of 34 delivered exactly `floor(n x start / lot)` lots: the wait bought the maker
  nothing. Fixed in the wallet (`cross-market.ts`: the auction starts at the whole lots the route pays for the order, warning
  `CROSS_WHOLE_LOTS_BELOW_BOUND` with fillable sizes when even those are below the worst bound) and in the bots (sized at the route's best
  level, not at the worst). Inherent: the rounding itself, mean 3.5 % of the delivery below `n x start` (TUSD>TETH 2.3 %, TETH>TUSD 4.9 %:
  4 to 9 TUSD lots per order).

## Redeploy on main eaa93db (2026-10-01, TN10): round-3 fixes

Rolled into the running soak (same keys, tokens, databases; no migration line) from 84d8efd: `merge --ff-only main` (9 commits: self-describing
record log, matcher waits for the indexer, pair-page fillability, pair-market auction starting at the whole lots the route pays, bot UTXO
consolidation), executor `--features deploy-tn10`, `build-wasm.sh --tn10 --web`, web `npm run build`, `build-ui.sh`, soak bundle (78 unit tests
green), `scripts/stop.ps1`, databases / binary / `wasm-node` backed up to `run/backup-pre-eaa93db/`, copies in, `scripts/start.ps1` at 10:25:55 UTC.
Each executor logged `waiting for the indexer to catch up` once and `the indexer caught up` 9 s / 13 s later; no maintenance step or sell ran before it.
`scripts/watch-stats.mjs --since <ISO>` (new, read-only: indexer API and executor logs) computes the auction positions and matcher income below.

Watch window 10:26 to 11:16 UTC (50 min, no restart of any child, 0 new incidents; the 6 listed in the report are older):

| | |
|---|---|
| bot UTXO consolidation | 372 merge transactions (`consolidate_*`, 8 inputs each, 0 conflicts, 0.109 KAS fee each = 40.7 KAS), 36,528 KAS of carriers freed. Market maker (balances before -> after): TUSD UTXOs 802 -> 88 (carriers 14,972 -> 880 KAS), TETH 545 -> 11 (6,082 -> 110 KAS), spendable KAS 1,063 -> 19,629; traders 60..98 -> 6..12 TUSD and 78..208 -> 7..12 TETH. Token amounts unchanged. By the end `token_utxos:mm:TUSD` 27, `:TETH` 13. The traders were at single digits after 12 min, the market maker TETH side at 19 after 32 min and TUSD at 27 after 50 min (at most 3 merges per wallet and token per minute) |
| skips | 0: `skipped=0` on all 264 ticks, 0 `skipped why`, 0 `ExceededCommittedScriptUnits`, 0 `insufficient funds`, 0 `operator funds are low` |
| pair-market auctions | 5 auction fills (all TUSD>TETH, 200 DAA window): position 0.000, 0.025, 0.034, 0.034, 0.105 (median 0.03, mean 0.04; 4 in the first decile, 0 at worst) at 0.0 to 2.1 s after the activation (median 0.7 s). The previous round: median 0.77, 0.7 to 21 s (median 15.5 s). 4 deliveries beat the start rate (route remainder to the maker) |
| matcher income per cross tx | avg 2.87 KAS, median 2.00 (n 36: single 2.73 on 34, two cross fills 5.28 on 2); a plain batch 0.082 (n 56). Previous round avg 2.86 (single 2.78, two-plus 3.81), plain 0.07 |
| cross volume | 38 cross fills in 36 transactions, checker invariant 11: 687/687, 0 below due, 177 route remainders delivered; cross orders in the window 66 (GTC / GTD 43, IOC pair market 16, FOK 7) |
| executors | exec-a finalized 41, exec-b 71 in the window, 0 rejected, every conflict a benign race (163 matcher, 27 bot) |
| x402 | 25 payments accepted in the window (native 8, kcc20 8, swap 9), 0 failed, 0 ambiguous |
| stops | arm / trail 84 -> 86 (ok 78 -> 80, unverifiable 7 unchanged), rearm 17 -> 18, repeat 1002 -> 1024; no trigger incident |
| resources | 541 MB total (exec-a 65, exec-b 61, bots 223, checker 112, ui 68, miner 12), peak 599; 0 restarts |

Incident: a network stall at 10:56 to 11:00 UTC (the Binance price poll timed out at 10:56:44; both executors' ticks timed out after 30 s; block
batches took 105 s and 115 s to fetch, lag 231 to 1,112 DAA). Both indexers caught up unaided at 11:00:08 / 11:00:19; during it
`the book lags the node; planning nothing until it catches up` held every matcher (exactly the new gate). No transaction was built on stale
state, no skip, no checker incident.

Findings:

* **Bot sizing of TETH>TUSD pair market orders is inverted (soak bots, `pair-math.ts` `bestLots`).** 11 IOC pair market orders TETH>TUSD in the
  window: 9 killed unfilled, 1 cancelled, 1 filled; none with an auction (`auction_daa` absent). TUSD>TETH: 5 of 5 filled at the auction's start.
  `bestLots` picks the size with the least `routeWaste`, i.e. `n x bestLot` just BELOW a whole number W of received lots (26 lots x 26.86M =
  6.98 TUSD, 22 lots 5.994, 15 lots 3.989): the route then pays only W-1 whole lots, `wholeStart` finds `(W-1) x lot / n` below the worst and
  gives no auction, and the fixed rate at the worst needs ceil(n x worst) = W lots, one more than the route pays: the order can never fill. All 11
  logged sizes show it (route pays 6, worst needs 7; 9, 10; 3, 4; 5, 6). The TUSD>TETH side hides it because a received TETH lot (6 KAS) is small
  against a sold TUSD lot. The fix is the other rounding: the least remainder ABOVE a whole number (`n x bestLot mod lot` small), so the route
  pays W whole lots at the start. The ticket's own `sizeCross` (exact multiple, else the typed size, with the `CROSS_WHOLE_LOTS_BELOW_BOUND` warning) is not affected.
* **The pair page note "Crossed by ...: matchers are catching up" stays up for minutes.** 140 samples every 20 s (10:28:51 to 11:15:12): crossed 93
  (66 %), the route-cannot-fill overlap note 47, 16 separate crossed runs, the longest 8.0 min and 7.7 min; the page flips between the two notes as
  the books move (31 changes). The classification works (a crossing the route cannot pay in whole lots reads as the neutral note), but a crossing the
  model calls fillable can rest for 5 to 9 min because the model leaves fees and the matcher's real batch out (a 2.9 to 5.5 KAS gross
  gain by the whole-lot rule, `dd2f7a0b` / `c09646a3` open 7 to 9 min); I did not establish what the matcher waited for there (the book kept moving; most of these orders did fill within minutes).

Fixed on `c8/xwait` (not yet rolled into the soak): **the matcher, not the page, was waiting.** The reference planner replayed over
the exec-a database every 50 DAA of the window (each past book rebuilt from `order_utxos` / `token_utxos` alive at that DAA score) predicts
the real fill instants of the cross limits to within 0 to 10 s (`c09646a3` 420 s after it became fillable vs 424 s on chain, `80b5f460`
1,340 vs 1,341, `b7de2458` 765 vs 769, `ee89aa3e` 1,125 vs 1,127). At 373 of 394 instants (95 %) some cross limit could be filled for a
margin of at least 0.06 KAS (best plain bids of A, whole asks of B) and the planner built nothing with it. Three planner defects:

* **Opposite directions blocked each other.** A TUSD>TETH cross limit and a TETH>TUSD one each took one leg first (their purchases rank
  above the plain bids), each then blocked the other's second leg (no token both sold and bought by cross limits) and the reconciliation
  dropped both. A resting TETH>TUSD cross limit that crosses at the best prices but never pays in whole lots (`05e7ef18`, `1dcaca2a`,
  `542ae6b3`, ... the "route overlap" of the pair page) thus held `dd2f7a0b` (5 lots, a 2 to 3 KAS margin most of the time) for 10 min. Now a leg
  enters only while its twin can follow.
* **Largest fill or nothing.** The planner took the largest fill the legs reach; when the last whole lot of B (23 KAS for TUSD) made it
  lose, the route was dropped whole although a smaller fill paid (`7c7bec5c`: 29 lots lose, 27 earn 6.1 KAS; `b00aa833`: 5 lose, 3 pay). Of the
  9 TETH>TUSD IOC orders killed unfilled in the window (above, "bot sizing"), 8 would have filled 19 to 35 lots on arrival (the ninth 3). Now every
  cross limit is bounded by the largest fill its route could pay on its own and left out when none can (an auction still relaxing is never cut).
* **Losing cross limits sank paying ones.** Small cross limits that lose on every fill took the best levels and the 4 / 4 cross slots
  first; dropping one only let the next take its place at the same loss, so no drop raised the profit and the batch was given up
  (`d895415a` behind `05e7ef18` / `7c7bec5c`, the IOC `1e8ceb30` behind `80b5f460` / `c7bd05cd`). Now those are left out up front, the
  losers that only take a dropped one's place are dropped with it, and each allocation is settled afresh from the drops.

Replay with the fix: 9 of 394 instants (2 %) leave a fillable cross limit unfilled, all with margins of 0.08 to 0.8 KAS that the per-unit
route price of the last partial lot of B, or the 4 / 4 cross caps, do not reach. The pair page's model was right (it judges whole lots and
the fee); not changed. Tests: `matcher_cross.rs` `opposite_cross_limits_never_block_each_other`,
`a_route_that_loses_on_its_last_lot_of_b_fills_fewer_lots`, `live_an_ioc_cross_limit_is_not_lost_behind_resting_ones_on_one_bid` (the
10:59 UTC book reduced to six orders; each fails on the old planner).

## Redeploy of the cross-planner fix (2026-10-01, TN10): 01648ef and a6613ee cherry-picked onto m9/soak

Rolled into the running soak without merging main (main also carries the cost pass that changes order templates): `git cherry-pick 01648ef`
(matcher: cross limits the route can pay no longer wait behind ones it cannot; applied cleanly, its README block above included) and
`a6613ee` (soak bots size TETH>TUSD pair-market orders to a whole received lot from above; bots only). Executor `--features deploy-tn10`
(private target dir, 2 jobs, 8 m 52 s), soak bundle `npm run build` (79 unit tests green), `scripts/stop.ps1` at 16:52:12 UTC, the previous binary and
bundle in `run/backup-pre-01648ef/`, `scripts/start.ps1` at 16:52:17 UTC. Same keys, tokens and databases, no migration line. Each executor
logged `waiting for the indexer to catch up` and `the indexer caught up` 4 s (exec-a) / 2 s (exec-b) later. Two read-only scripts added:
`scripts/cross-wait.mjs` (cross orders from the indexer API: wait from placement to first fill, fate of the pair-market IOC orders) and
`scripts/pair-note-sample.mjs` (Playwright; the pair page's spread line every 20 s, `--summary` for the crossed share and the run lengths). `scripts/cross-flow.mjs --from <ISO> --to <ISO>` (cross fills per hour, orders placed with the received tokens their full fill delivers, open cross orders and leftovers whose `amount_left` is below the order's `min_fill`; under v2.6: whose remainder delivers less than one received lot).

Watch window 16:52:17 to 17:46:04 UTC (53.8 min). Baseline = the previous binary over its whole run (10:25:55 to 16:52 UTC, 6.4 h, same script) and the
50-min window of the last round.

| | previous binary | with 01648ef + a6613ee |
|---|---|---|
| pair page "matchers are catching up" (samples every 20 s) | 140 samples 10:28 to 11:15: crossed 93 (66 %), 16 runs, longest 8.0 and 7.7 min | 165 samples: crossed 58 (35 %), the neutral route-overlap note 107; 23 runs: 10 of one sample, the rest 40 to 140 s, longest 2.3 min twice (median run about 1 min); 45 changes between the two notes |
| cross limits (non-IOC) wait, placement to first fill | last round median 64 s, 10 above 5 min; same script, 10:26 to 11:16: 32 of 53 filled, median 159 s, 11 above 300 s; whole run: 268 of 463 filled, median 77 s (GTC 115 s), p90 755 s, 69 above 300 s, max 5,204 s | 35 of 60 filled, **median 2.7 s**, p90 107 s, max 263 s, 4 above 60 s, **0 above 300 s** |
| TETH>TUSD IOC pair-market orders | 1 of 11 filled (9 killed unfilled); whole run 17 of 88 had a fill | 13 orders: 8 filled whole, 5 killed with 23 to 35 lots filled (35 of 37 twice over, 23 of 24; the unfilled remainder refunded), none killed without a fill; 7 of 13 with an auction |
| TUSD>TETH IOC pair-market orders | 5 of 5 filled; whole run 80 of 86 | 13 of 13 filled |
| pair-market auction fills | 5, median 0.7 s after activation | 20, median 0.8 s (0.1 to 19.1 s), 12 in the first decile, 0 at the worst rate, 20 deliveries with a route remainder |
| matcher income per cross tx | avg 2.87 (median 2.00; n 36); whole run avg 3.06, median 2.54 (n 358, 8 % with 2+ cross fills) | avg 2.86, median 1.67 (n 67, every one a single cross fill); plain batch 0.058 (n 84) |
| cross volume | 36 cross transactions in 50 min | 67 cross fills in 67 transactions in 54 min; checker invariant 11: 1,104/1,104 (node 1,041), 0 below due |
| executors | 41 / 71 finalized, 0 rejected | exec-a 76, exec-b 89 finalized, **0 rejected**; 211 matcher conflicts, every one a `MissingInput` or `DoubleSpend` race (the loser backs off), none a failure |
| incidents | 0 new | 0 new (6 listed, all older), 0 warn / error lines in either executor log, 0 restarts of any child |
| resources | 541 MB (peak 599) | 603 MB (peak 620): exec-a 60, exec-b 39, bots 289, checker 131, ui 72, miner 12 |

Reading:

* **The matcher fix does what the replay said.** A cross limit that gets a fill gets it in 3 s at the median; the 5 to 87 min waits are gone. The
  plain count of "with a fill" is the same as before (58 %), because the others are cancelled, killed or still resting off the route's price (8 cancelled before a fill is possible, 8 killed, 5 refunded). The four open for 17 to 35 min (`bc6f7098`, `769be339`, `bb606d0c` TUSD>TETH at 36,947 to 37,300
  TETH units per TUSD, `ee658437` TETH>TUSD at 0.27176 TUSD per TETH lot) are out of the money by hand: at 17:47 the route (TUSD bid 23.77 KAS,
  TETH ask 6.4616 KAS per 1e4 units) pays 36,779 units per TUSD (6 TUSD: 22 whole lots against the 22.38 the order needs) and 0.2709 TUSD per
  TETH lot, 1.4 % and 0.3 % short before fees. The pair page calls those the neutral overlap note, as it should.
* **The TETH>TUSD IOC orders now fill.** Together the two fixes: the bot sizing (the route pays the whole lots the order asks) and the planner
  (a smaller fill that pays is no longer dropped for the largest one that loses): 37-lot orders fill 35 lots and refund the other 2 instead of
  being killed whole. Which of the two did how much was not separated (both went in at once). The 5 "killed" are these partial fills, the order's
  state after the IOC deadline, not unfilled orders.
* **The page still shows "catching up" a third of the time**, in runs of up to 2.3 min (before: 8 min): crossings the model calls fillable that a matcher
  fills within a block or a few. I did not look for what is left of the 20 to 140 s (the 9 of 394 replay instants with a margin under 0.8 KAS is
  the known remainder).
* **No transaction with two cross fills in the window** (before 8 % of the cross transactions): not looked into; the twin rule only lets a
  leg in while the opposite one can follow, and the window is short (67 cross transactions).
* The remaining DoubleSpend / MissingInput lines are the usual two-matcher race (both executors plan the same batch; the loser backs off), 3.9 per minute
  against 3.3 before (163 in 50 min).

## Redeploy on main ae87d77 (2026-10-02, TN10): cost passes 1 + 2, liveness fixes, intents / invoices

Main moved from the soak's dc0169f to d5a911c and then ae87d77 (`merge --ff-only`): order templates changed (cost pass 1: new hashes, same
state layouts), plain asks are amended IN PLACE through the cancel path (cost pass 2), KOB1 payload v3 (compact placement record, AMEND record),
no tiny change outputs in amends, the cross-planner fix, the liveness fixes (keepers back off, intents expire, retired-template cancel
policy, wallet recovery), a new router id, x402 intent payments and invoices, and (ae87d77) 264 more budget-table fill branches. Executor
`--features deploy-tn10`, `build-wasm.sh --tn10 --web`, web `build:wasm`, soak bundle (80 unit tests), `build-ui.sh`; fresh executor
databases from the sink; same keys, TUSD and TETH.

### Wind-down under a TN10 transaction flood

TN10 was flooded from about 19:26 UTC on 10-01 (blocks of ~300 transactions, bursts of 860; 550 to 1,300 accepted tx/s against the
1.2 to 1.8 MB/s this PC gets from the node). At 20:13 / 20:12 UTC both indexers fell behind: each VSPC batch the node returned grew (up to
198k transactions, ~300 MB) until it no longer arrived within the follower's 180 s request timeout, and from then on the same request timed out
every 3.5 min (exec-a `timeout after 180s attempt=59`, cursor frozen at DAA 585,538,086 for 3.4 h). The matchers held correctly (`the book
lags the node; planning nothing`), but the bots kept trading against the frozen book: 216 bot transactions (placements, amends, cancels,
fan-outs) went on chain unindexed. The previous binaries could not be caught up (the gap was ~10 GB of VSPC data at < 2 MB/s, against ~1 MB/s of
new chain), so the wind-down rebuilt the state instead, with the OLD binaries throughout:

1. `scripts/stop.ps1`; databases, logs, binaries, `wasm-node`, `web-dist` and the old bundle archived in `run/backup-pre-d5a911c/` (state at
   the stall; the state after the wind-down in `backup-pre-d5a911c/after-winddown/`).
2. **The gap transactions located on the node** (no tx index): the block hashes after the old cursor (`getBlocks`, 125k hashes), then per bot
   transaction of `run/state/txs.jsonl` the block holding it: by the accepting DAA of its unspent change (UTXO `blockDaaScore`: the accepting
   chain block, then its mergeset), else by the acceptance delay of the bot transactions submitted within 90 s (the mempool is FIFO at equal fee
   rates; delays were 0.2 to 10 min during the flood). 236 transactions located, including 30 submitted before the cursor but accepted after
   it; 7 not (6 KAS-only bid placements, 1 cross-limit cancel).
3. `kob-executor index rebase --to sink` (old binary): 69 tracked outputs closed as spent in the gap. Their KOB1 records (old wasm
   `decodePayload`) gave a maker export of the 182 orders the gap transactions created; `index import-orders` verified and imported all 182 on
   the node. The token change of the gap transactions was recovered by matching every token output against key-owned KCC-20 states (owner = a
   soak key, amount = an 8-byte word of the leader's `nextStates` push) and seeded into the bots' trackers.
4. exec-a alone (old binary, `rpc_timeout_secs = 900`) followed from the sink; its keepers refunded / killed 83 orders in the first minute (IOC /
   FOK / expired gap orders); the rest was cancelled with the old bundle's `soak.mjs cancel` (maker keys, the web cancel path) in 4 rounds: 77
   cancels, 10 "order is gone" (filled / killed meanwhile). **0 open or partial orders** (indexer and `export-orders --live-only`), 0 strays.
5. Holdings verified on the node (`scripts/holdings.cjs`, new: indexer + trackers, each P2SH checked) and written into the trackers: TETH 1,000
   of 1,000, TUSD 9,999,965.25 of 10,000,000; the missing 34.75 TUSD is most likely the custody refund of the unlocated cross-limit cancel (t4,
   22:39 UTC).

Per key (KAS: the node's total at the key address before the wind-down at 23:56 and after it; tokens: verified holdings after it, amount
(UTXOs)):

| key | KAS before | KAS after | TUSD | TETH |
|---|---|---|---|---|
| bank | 230,274 | 230,274 | 4,300,000 (1) | 300 (1) |
| mm | 10,995 | 13,453 | 3,005,301 (12) | 300.1305 (17) |
| t1 | 23,585 | 25,392 | 498,878.56 (21) | 79.9835 (24) |
| t2 | 28,839 | 29,125 | 498,724.49 (19) | 79.9799 (19) |
| t3 | 23,356 | 23,856 | 498,961.37 (16) | 79.9799 (20) |
| t4 | 15,197 | 18,400 | 499,290.75 (32) | 79.9498 (18) |
| t5 | 16,039 | 17,918 | 499,288.88 (13) | 79.9763 (26) |
| payer | 9,205 | 9,205 | 199,389 (4) | 0 |
| merchant | 809 | 809 | 130.25 (203) | 0 |
| execA / execB | 3,761 / 4,332 | 3,796 / 4,332 | 0.63 / 0.34 | 0 / 0.0001 |

KAS rose by what the cancels and refunds returned from bid escrows and carriers. Token UTXOs were not merged before the swap: the bots'
consolidation (unchanged token program) merged them after the start (66 merge transactions in the window); the merchant's 203 payment UTXOs
stay.

### New run

Fresh databases from the sink (`--start sink`), `rpc_timeout_secs = 900` in both `executor.toml` (the flood), exec-a's facilitator with
`intents` (keeper = exec-a's key) and `invoices` (config `x402.invoiceIntent`, supervisor), started 01:34:40 UTC; restarted 02:25:22 UTC after
re-seeding the market maker's tracker (finding below). Watch window **02:29:32 to 03:31:00 UTC** (61.5 min; executors up since 02:25:22, no
restart; bots since 02:29:32; checker restarted 02:51 for the amend fix). Baseline = the previous binaries 16:52 to 19:20 UTC on 10-01 (2.5 h,
before the flood). `scripts/fee-compare.mjs --before 2026-10-01T16:52:17Z 2026-10-01T19:20:00Z --after 2026-10-02T02:29:32Z 2026-10-02T03:31:00Z`.

| | before | this run |
|---|---|---|
| ask amends | cancel-replace, 9,708 B, 0.01971 KAS (n 476) | **in place**: 195, all 1.5 to 2.5 KB, avg 2,022 B, 0.00404 KAS (-79 % bytes, -80 % fee) |
| bid amends | 2,400 B, 0.00531 KAS (n 580) | 139, 1,970 B, 0.00467 KAS (no tiny change) |
| placement payload (KOB1 v3) | 323 B per ask record before cost pass 2 (c1 report) | plain orders 140 to 159 B per transaction, cross limits 208 to 213 B, IFD / IFO 429 to 466 B (entry + exit) |
| bot fees | 22.25 KAS/h, 1,268 tx/h, 0.01755 KAS/tx, 8,515 B/tx | 17.33 KAS/h, 1,133 tx/h, 0.01529 KAS/tx, 7,404 B/tx: **-12.8 % per tx**, -22.1 % per hour; the run's action mix at the old per-action fees: **-17.4 %** |
| matcher income per cross tx | avg 2.86 KAS, median 1.67 | avg 2.12, median 0.77 (n 61, all single cross fills); plain batch 0.104 (n 86); finalized exec-a 98 (90.07 KAS), exec-b 90 (49.63 KAS) since 02:25 |
| cross limits (non-IOC) | median 2.7 s, 0 above 300 s (n 35) | 72 orders, 31 filled: median 6.7 s, p90 732 s, 5 above 300 s (not examined for fillability) |
| pair-market IOC | TETH>TUSD 13: 8 filled, 5 partial; TUSD>TETH 13/13 | TETH>TUSD 16: 8 filled, 8 killed after partial fills (35/37 x4, 31/37, 15/16, 23/26, 7/10, 5/8) or none (0/37); TUSD>TETH 12: 10 filled, 2 killed |
| pair-market auctions | 20 fills, median 0.8 s | 18 fills, position median 0.07 (11 in the first decile, 0 at worst), 0.1 to 13 s after activation (median 1.5 s), 18 with a route remainder |
| stops | | arm / trail 18 checked (16 ok, 1 unverifiable warn), rearm 8, repeat 114 |
| keepers | | 35 refund / kill transactions (exec-a 19, exec-b 16); back-off is not logged |
| x402 | | 28 settlements in the window (native 7, KCC-20 7, swap 7, **invoice + intent 7**), 0 failed; since the start 11 invoices paid by intent (5.3 to 14.6 s each, 2 TUSD locked as the worst case); facilitator `kob_x402_intent_executions 9`, `_conflicts 2` (re-planned), `_failed 0`, `kob_x402_invoices_registered 7`, `_refused 0` |
| executors | 0 rejected | **0 rejected** (`kob_matcher_rejected_total 0` on both); 286 matcher + 45 bot conflicts, all `MissingInput` / `DoubleSpend` races |
| resources | 603 MB | 707 MB (peak 739): exec-a 83, exec-b 75, bots 340, checker 119, ui 71, miner 19 |

Incidents (24 in the run; each one explained in `run/incidents.explained.jsonl`):

* **21 `all-in` errors + 1 `trigger` error: checker false positives.** An ask amended in place keeps its covenant id with new terms (price,
  tif, expiry, activeFrom, tipLot); the indexer records `amend` events with `detail.previous`. The checker compared fills with the terms it had
  memoized before the amend: all 21 fills were at their amended price (e.g. `9440babb`: 2,456,750,000 -> 2,443,640,000 -> 2,429,590,000, filled
  at 2,429,590,000), and the trigger "evidence above the stop" was ask `ae5f90e7`, amended from 655,720,000 to 650,980,000 at DAA 585,741,363
  and filled at 650,980,000 <= stop 651,490,000 in the arming transaction (exposed 324 DAA). Fixed on this branch: `termsAtFill` (the
  `previous` of the first amend after the fill, else the order's current terms; a closed order takes the view's `price`, `tip_lot`, ...) for
  fills and for trigger evidence, with a regression test of the live case.
* **1 `supply` error: the market maker's tracker lost its pre-run holdings.** A fresh indexer never lists holdings it did not see created, so
  the bots find them only in `run/state/tracker-<key>.json`. Between 01:34 and 02:04 the market maker's store was emptied although its
  3,002,049 TUSD and 297.9 TETH UTXOs were still unspent on the node: 151 / 46 `INSUFFICIENT_TOKENS` refusals of its sells, and the checker's
  TETH supply fell by 300. Replaying the tracker's resolution on the same candidates verifies them (not reproduced); not root-caused.
  Re-seeded at 02:25 from `scripts/holdings.cjs` (indexer + trackers + the wind-down list, node-verified), supply baseline reset; a store that
  loses more than half of its candidates at once is now logged with its caller (7 such lines after 02:25, all spent UTXOs: no live UTXO
  missing at 03:00). TUSD baseline -34.75 TUSD (the unlocated gap refund above), TETH 0.
* 1 `trigger` warn (03:26): evidence not verifiable from the indexer data (as before).
* **Network stall 03:29 to 03:33 UTC**: a 21k-transaction batch took 124 s to fetch (0.25 MB/s), exec-a `tick failed error=timeout after 30s`,
  one `/native` payment `payment_pending` in that minute; both indexers caught up by 03:34 unaided.

Findings:

* **Follower livelock under load (kob-executor).** `getVirtualChainFromBlockV2` has no size bound on the request side (the node caps `added`
  by chain blocks, not bytes), and a timed-out request is retried unchanged, so a follower that falls behind under a flood asks for an ever
  larger batch and never completes one (10-01 20:15 to 23:37 UTC: 60 identical timeouts). Raising `rpc_timeout_secs` helps only while the link
  outruns the chain; JSON at verbosity `High` costs ~1.5 KB per transaction, so following TN10 at 1,000 tx/s needs ~1.5 MB/s per indexer, which
  this PC does not get from the remote node. Needs a node next to the indexer, or a smaller wire format (Borsh wRPC, a compressed or filtered
  feed), and a follower that can bound or split a batch.
* **The bots trade on a frozen book.** During the stall the bots kept placing, amending and cancelling for 3.4 h against the stale indexer,
  which is what made the wind-down hard. A soak-side guard (no trading while `/v1/health` is not `following`) is cheap; not added in this round.
* **The record log has no single-writer lock.** `index import-orders` run while `run` was running on the same data directory interleaved two
  writers and broke the record log's hash chain (`record log frame 19796 is corrupt: chain hash mismatch`); both `run` and the CLI then refused
  the directory and the wind-down finished with `--no-record-log` (that database was discarded). Operator error first, but the data directory
  should be locked.
* The bots keep no placement records and do not call `trackFromBuilt`: a gap in indexing hides their own orders and token change until an
  indexer sees them (the web app records both).

Tools added: `scripts/fee-compare.mjs` (the bots' fees, sizes and payloads per action between two windows, and the cost change at a fixed
action mix) and `scripts/holdings.cjs` (node-verified holdings per key; `--seed` writes the trackers).

### Follow-ups (branch c11/load, not yet rolled into the soak)

* **Follower livelock fixed.** Every VSPC request is bounded to a window of blue score through `minConfirmationCount`
  (`rpc::window`): a timed-out batch is split, a fast one grows, a request is never repeated unchanged after a timeout, and a
  bounded batch keeps the state `catching_up` (the matchers keep waiting). `/v1/health` and the new `GET /v1/metrics` report
  `lag_blue`, `batch_window_blue`, the last batch, `bytes_per_tx` and `vspc_timeouts_total`. Measured 10-02 during the flood:
  1,354 B per accepted transaction at `High` (1,182 at `Low`): following 1,000 tx/s takes 1.35 MB/s per indexer, about what
  this PC gets from the remote node, so a fallen-behind indexer here still cannot catch up while the flood lasts
  (`docs/ops/executor.md`, Part B 3, *Bandwidth*).
* **Single-writer lock** on the data directory and the record log (`kob-writer.lock`); `index export-orders` opens read-only.
* **Bots pause while their indexer is not following** (`src/indexer-gate.ts`, enforced in `BotWallet.submit`; the mm, trader and
  x402 loops wait; the bank's KAS payouts are not gated). Logged as `indexer not following: trading paused` / `indexer following
  again: trading resumed`, stats `gate_*`.
* **The market maker's tracker loss, root-caused:** the tracker forgot a local candidate as soon as the indexer listed it. At 01:29
  UTC a `balances` run of the old bundle against the OLD indexer (which listed all 29 of the mm's seeded UTXOs) wrote the mm's
  store back empty; the fresh indexer of the new run never listed them. The checker's supply baseline at 01:37:43 (-3,005,563.35
  TUSD = the seeded candidates the old indexer had listed + the 34.75 gap refund) confirms it. Fixed in `web/src/data/token-tracker.ts`:
  indexer-listed candidates are kept; a candidate the node does not list is pruned only after 3 misses spanning the grace period
  (an empty node answer counts for nothing); an unreadable store is copied aside before a write. `FileStorage` reloads a file
  another process changed and writes only its own key (`src/file-storage.ts`).
* **Cross limits waiting > 300 s:** 4.9 of the 5 were not fillable (the KAS route paid at their limits only for seconds, median
  shortfall 1 to 1.4 %, replayed at 1 s steps and through the planner offline). One planner defect cost about 28 s (02:58:52 to
  02:59:20, `e7302001`): the profit step dropped at most 8 losing substitute cross limits behind a paying one; with 10 or more
  resting on one bid it gave the whole batch up and built nothing. Cap removed (`matcher_cross::live_a_paying_cross_limit_...`).

## Redeploy on main 6b78df6 (2026-10-02, TN10): if-done template revision, bounded followers, indexer gate

Main moved from ae87d77 to 6b78df6 (`merge --ff-only` of m9/soak, which main already contained): the if-done entries re-pinned
(`KobIfdBid` 35ed6bed -> ca757658, `KobIfdAsk` 789ddaa1 -> 138d4fd8, KRON twins d16e03ff -> 354fb5de, 6d19a13d -> fd13762a; the four
retired artifacts in `contracts/retired/`), 12 router token actors re-pinned (KOBOrders 554fbc99, KobRouter cf3ad2f8), the per-program
token-output floor and dust refusal, the bounded adaptive VSPC windows of both followers, the single-writer lock on the data and record
directories, the cross profit step without a cap, the token tracker that never drops holdings on weak evidence, the bots' indexer gate
(`src/indexer-gate.ts`) and the x402 SIGHASH_ALL checks. Every other template hash is unchanged (`templates()` of the old and new
kob-wasm compared: KobAsk, KobBid, KobCondAsk, KobCondBid, KobCross, the token programs and their KRON twins are identical), so only
if-done positions had to end before the swap. Executor `--features deploy-tn10`, `build-wasm.sh --tn10 --web`, web `build:wasm`, soak
bundle (101 unit tests, typecheck), `build-ui.sh`. **Databases kept** (same schema version; old if-done rows stay readable under their
old hash: `0d800537`, a cancelled `KobIfdAsk` 789ddaa1, reads back from the new API), trackers re-seeded from `scripts/holdings.cjs`
(node-verified), same keys, TUSD and TETH.

**Supervisor: `rpcTimeoutSecs`.** The supervisor rewrites `executor.toml` on every start, so the `rpc_timeout_secs = 900` of the previous
run was lost at its 02:25 restart: this run's first stall (below) was again `timeout after 180s`. The timeout now comes from the config
(`rpcTimeoutSecs`, global or per executor) and is written into `executor.toml`; 900 for the wind-down, **180** (the default the bounded
follower is designed around: batch target = timeout / 6) for the new run.

### Wind-down (old binaries throughout)

TN10 was flooded again. At 04:49:57 UTC both indexers stopped (the last batch: 879 chain blocks, 335,599 transactions, 174 s; then
`timeout after 180s` every 3.5 min) and, as on 10-01, the bots kept trading against the frozen book: 417 bot transactions by 06:05
(19 of them if-done / IFO / repeat placements), plus three invoice intents the facilitator could not execute in time. The flood paused
at about 06:00 (89 tx/s, mempool 2):

1. 06:05 `scripts/stop.ps1` (bots off from here on: no placements of any kind); `run/state/tracker-*.json` and the old `dist/` copied to
   `run/winddown-6b78df6/`.
2. exec-a alone with `rpc_timeout_secs = 900`: it caught up in 18 min (06:06 to 06:24: 756,644 transactions in 1,107 s of fetches,
   batches of 1,000 to 1,300 chain blocks); exec-b followed (caught up 06:47). No rebase, no import: every gap transaction is in both
   databases.
3. Intents: all 29 stored invoices expired; the three intents created during the stall (05:09, 05:18, 05:25 UTC, 2 TUSD each) are
   `failed / intent_expired` in the ledger and their locked TUSD is spent on chain (`/v1/token-utxos?owner=<intent id>&spent=true`); no
   pending ledger entry. `x402.invoiceIntent.enabled = false` for the rest of the wind-down.
4. If-done positions cancelled with the OLD bundle's `soak.mjs cancel` (makers' keys, the web cancel path): 17 entries (13 `KobIfdBid`
   35ed6bed, 4 `KobIfdAsk` 789ddaa1) and 15 booked exits in one round (32 cancelled, 0 refused), then one exit booked by a fill in the
   meantime. **0 live if-done entries on the four retired hashes and 0 live exits, on both indexers** (06:47). The 265 other live orders
   (plain, conditional, cross; 177 of them market-maker bids placed against the frozen book) stay: unchanged templates.
5. Balances (`soak.mjs balances` of the old bundle, before 06:28 / after 06:48) and holdings (`scripts/holdings.cjs`, node-verified):

| key | KAS before | KAS after | TUSD before -> after (UTXOs) | TETH before -> after (UTXOs) |
|---|---|---|---|---|
| bank | 260,540 | 264,244 | 4,300,000 -> 4,300,000 (1) | 300 -> 300 (1) |
| mm | 210 | 210 | 3,005,354 -> 3,005,354 (17) | 300.164 -> 300.164 (15) |
| t1 | 21,737 | 22,736 | 498,921.5573 -> 498,923.5573 (30) | 79.9641 -> 79.9643 (24) |
| t2 | 28,772 | 29,050 | 498,629.4913 -> 498,631.4913 (14) | 79.9938 -> 79.9945 (25) |
| t3 | 22,696 | 23,130 | 498,989.3667 -> 498,996.3667 (22) | 79.9513 -> 79.9516 (31) |
| t4 | 19,171 | 19,300 | 499,278.7454 -> 499,281.7454 (18) | 79.9341 -> 79.9341 (26) |
| t5 | 17,125 | 17,611 | 499,235.8771 -> 499,235.8771 (16) | 79.9762 -> 79.9762 (15) |
| payer | 9,523 | 9,523 | 199,345.75 (18) | 0 |
| merchant | 1,234 | 1,234 | 137.5 (232) | 0 |
| execA / execB | 4,142 / 4,543 | 4,142 / 4,543 | 0.6267 / 0.3355 | 0 / 0.0001 |

KAS rose by the bid escrows and carriers the cancels returned (the bank by its coinbase); the traders' tokens by the custody the ask-side
entries returned. The market maker holds 210 KAS at its address: the rest sits in its 177 resting bids (a stall artefact; its first
ladder pass after the start cancels them, see below). Holdings total 9,999,907.25 TUSD and 999.9848 TETH; the rest is in the custody of
live orders.

The payer's `payment_failed/token_conservation` ("the payer holds 0 units of the pay token") from 05:35 on was the stall, not a loss:
the payer held 199,345.75 TUSD in 18 node-verified UTXOs throughout.

### New run

Executors and UI started 06:49:00 on the kept databases (both `following`, lag 0 within 15 s), trackers seeded, then the whole soak with
`invoiceIntent` on at **06:49:43 UTC**. Watch window 06:49:43 to 07:51 (61 min), sampled every minute (`/v1/health` of both indexers and
the chain's transactions per block from the node).

**TN10 flooded again from 07:00 and the indexers do not keep up.** The chain carried 50 to 130 tx/s until 07:00, 400 to 1,400 tx/s
until 07:10, and **2,500 to 7,600 tx/s from 07:11** (mempool up to 28,600). The bounded follower does what it was built for: **no VSPC
timeout** (`vspc_timeouts_total 0` on both), the window adapts between 37 and 600 blue score (mostly 74 to 148), every batch arrives in
6 to 103 s (median about 20 s) with 5,000 to 82,000 transactions during the flood, and the cursor never stops. But each indexer gets about 1,500 accepted
transactions per second over this link (~2 MB/s at the 1.35 to 1.6 KB per transaction measured: `bytes_per_tx` 1,600 to 1,700 at the
end of the window, higher early on while blocks were near-empty), against a chain producing 4,000 to 7,500: the lag grew from ~240 DAA at
07:03 to **13,600 DAA (23 min) at 07:51**, about 400 DAA per minute, `lag_blue` equal to it. As documented (`docs/ops/executor.md`, Part B 3,
*Bandwidth*), no batching catches up when the link is slower than the chain; this needs the indexer next to its node.

| | ae87d77 run (02:29 to 03:31, flood-free) | this run (06:49:43 to 07:51) |
|---|---|---|
| indexers | following | following until 07:00; `catching_up` from 07:10, lag 13,600 DAA at 07:51, 0 timeouts, no livelock |
| bot trading | 61 min | **21 min**: the gate paused the bots at 07:01:57 (448 s), 07:10:17 (10 s) and from **07:10:54 to the end** (lag 240 DAA and growing); 0 bot transactions after 07:10:48 |
| bot transactions | 1,161, 17.76 KAS | 357 (13 conflicts, 0 failed), 5.91 KAS; per tx 0.01654 KAS / 8,129 B (+8 %: the mm's first passes cancelled its stall bids one by one, 90 `mm_cancel` per hour); same action mix at the old per-action fees +3.3 % |
| if-done on the new templates | | 10 entries (7 `KobIfdBid` ca757658, 3 `KobIfdAsk` 138d4fd8, incl. 1 repeat IFD and 1 repeat IFO): 3 fills (1 partial), 5 cancelled, 2 open; 3 exits booked, 1 filled. 0 orders created on a retired hash |
| x402 | 28 settlements, 7 invoice + intent | 9 settlements, 0 failed (native 3, KCC-20 2, swap 2, **invoice + intent 2** on the new router cf3ad2f8, 5 and 8 s); `kob_x402_intent_executions 2`, `_conflicts 0`, `_failed 0`, `kob_x402_invoices_registered 2`; the payer loop waits on the gate after 07:09 |
| cross limits | 72, median 6.7 s | 11 (8 GTC), 2 filled after 9.3 / 10.6 s; pair-market IOC 6: 1 filled, 5 killed (auctions held 4); the rest waited on the frozen book |
| executors | 0 rejected | **0 rejected** (`kob_matcher_rejected_total 0` on both), 0 ERROR lines; finalized exec-a 25 (19 match, 6 refund, 10.72 KAS), exec-b 26 (20 match, 6 refund, 5.99 KAS); 67 lost races (`MissingInput` / `DoubleSpend`); both matchers `the book lags the node; planning nothing` from 07:10 |
| resources | 707 MB | 896 MB (peak 997): exec-a 207, exec-b 196 to 228, bots 307 to 378, checker 120, ui 47, miner 19 |

The gate logged each transition once (`indexer not following: trading paused` with the state and lag, `indexer following again: trading
resumed` with the pause length): 4 pauses, 3 resumes. Since 07:10:54 no bot has touched the chain, which is exactly what the gate is for:
on 10-01 and again on 10-02 04:49 to 06:05 the bots had placed 216 and 417 unindexed transactions in the same situation.

Incidents (9 in the window, each explained in `run/incidents.explained.jsonl`):

* **8 `health` errors** (07:04 to 07:50): `indexer unhealthy: state catching_up` (with the lag from 07:07): the flood, as above.
* **1 `ioc-fok` error** (07:17): FOK ask `317f1a23` (t2), placed at DAA 585,930,616 (07:10:47) seconds before the gate closed; its kill
  DAA passed while both indexers were catching up, and keepers plan nothing while the book lags the node, so nobody can kill it before
  the indexers catch up. The checker measures the overdue against the NODE's DAA; against the indexer cursor it would not be overdue yet.

Other observations:

* `token tracker store shrank` (6 lines, 07:29, t1 to t5 from `fanOut`): the seeded candidates the traders' consolidation had merged away
  in the first 20 min, pruned after 3 misses over the 15 min grace. Checked on the node: every live UTXO the trackers no longer hold is
  listed by the indexer (0 missing); t1 keeps 6 of its 54 seeded outpoints and the node lists exactly those 6.
* `bank tick failed: Mass calculation error` (1 in this run, 80 in the previous one since 05:25): the bank's SDK payout generator; not
  examined.
* The UI shows `Indexer: catching up, lag 26 min` and the book banner `may be out of date` (screenshots `202610020754-*`).

Findings:

* **Flooded TN10 still defeats this PC's link** (as the follow-up note predicted): the follower fix removes the livelock and keeps the
  indexer honest (bounded, progressing, `caught_up = false`), the gate keeps the bots off the chain, but the soak is idle while the flood
  lasts. Running the indexers next to the TN10 node is the only fix; a second node for exec-b would at least halve the link's share.
* **The gate and the matchers wait for `caught_up`.** Under a moderate load (400 to 1,400 tx/s, 07:00 to 07:10) a batch takes 10 to 30 s
  and the indexer flips to `catching_up` at 150 to 300 DAA of lag, which paused the bots for 448 s although the book was at most ~30 s
  old. A tolerance (trade while the lag is below, say, 300 DAA) would keep a market usable at such loads; whether that is safe for the
  matcher is a design decision, not changed here.
* **The checker's IOC / FOK deadline** should be measured against the indexer cursor (or suspended while an indexer lags), not against
  the node's DAA: keepers cannot act before their indexer has seen the order.

Operate: `"rpcTimeoutSecs": <s>` in `run/config.json` (or per executor in `executors[]`) sets `rpc_timeout_secs` in every `executor.toml`
the supervisor writes. Wind-down files in `run/winddown-6b78df6/` (balances, holdings, cancel logs, the old bundle); the state before the
swap in `run/backup-pre-6b78df6/`.

## Redeploy on main 29f0c64 + lag tolerance (2026-10-02, TN10): parallel window fetch, bank and checker fixes

`m9/soak` merged `main` 29f0c64 (parallel window fetch for the indexer; no template change since 6b78df6, so the databases, tokens and
orders carried over). Executor `--features deploy-tn10` (private target dir, 2 jobs, 9 m 15 s), `scripts/stop.ps1`, the previous binary and
`config.json` in `run/backup-pre-29f0c64/`, copy, `scripts/start.ps1` at **08:38:36 UTC**: both executors resumed their databases, no
migration line. A second executor build (29f0c64 plus the lag tolerance, incremental 2 m 25 s) went in at **09:01:24** (previous binary in
`run/backup-pre-0f79591/`). The bots and the checker were restarted at 09:27 and 09:34 for the soak-side fixes below.

Operate (supervisor, `run/config.json`, global or per executor in `executors[]`): `"fetchParallel": 4` and `"prefetchMaxMb": 256` write
`fetch_parallel` / `prefetch_max_mb`, `"maxLagSecs": 30` writes `max_lag_secs` into every `executor.toml` (the supervisor rewrites it on
each start) and is also the bots' gate tolerance. 4 is the docs' value for a remote node; with two executors that is 8 connections to
the one TN10 node.

### Parallel fetch: catch-up after the TN10 flood (watch 08:40 to 09:25, minute samples)

The flood had ended by about 08:00 (tip blocks of 13 to 25 transactions, 130 to 390 tx/s measured at the node from 08:40; the mempool still
held 44,000 transactions at 09:26, see the latency note below), but the indexers were 26,800 blue score (45 min) behind and the history
they had to read was the flood: 1,000 to 1,250 accepted transactions per chain block (10,000+ tx/s) until about 08:48, calm after.

| | before (this run, 08:24 to 08:38, one connection) | after (08:40:56 to 08:54:25, 4 connections each) |
|---|---|---|
| exec-a | 786 tx/s, 1.05 MB/s; the earlier run's best was about 1,500 tx/s | **2,331 tx/s, 3.19 MB/s** |
| exec-b | about the same | **2,122 tx/s, 2.97 MB/s** |
| both together on the link | about 2.1 MB/s | **6.2 MB/s** (docs: one remote indexer reaches about 4 MB/s over 4 windows) |
| lag | 26,988 blue at 08:38, growing at the flood's rate | 26,806 to 3,535 (exec-a) and 26,172 to 15 (exec-b) in 13.5 min |
| `bytes_per_tx` | 1,317 to 1,330 | 1,310 during the flood, 1,390 to 1,430 after (fewer transactions per block) |

* Per minute the rate is bursty (windows are applied in order as they land): 0 to 7.9 MB/s, a 114 s and a 117 s fetch at 08:46 to 08:47
  (exec-a, 4 windows in flight, the two indexers sharing the link), then 20 to 40 s. **0 VSPC timeouts, 0 prefetch discards on exec-a, 1 on
  exec-b**; the batch grew from 16 to 1,024 chain blocks as the blocks emptied (127 blue score per second at the end).
* Both indexers were `following` at **08:54:25** (16 min after the start), `caught_up` true; the gate logged `indexer following again:
  trading resumed pausedSec 959` at 08:54:35. They have stayed within 10 to 170 DAA since.
* Cost: prefetch peaked at **341 MB (exec-a) and 327 MB (exec-b)**, 27 % over the 256 MiB (268 MB) budget (the estimate for windows in
  flight is the average bytes per chain block so far, and the flood's blocks were 10x the calm ones); RSS peaked at 955 and 769 MB
  (207 and 196 to 228 MB before), the soak total at 2,134 MB (about 1,000 MB before). After the catch-up RSS fell back to 86 and 83 MB
  at the second start. CPU was never the limit (33 s and 27 s of CPU for the whole catch-up).
* Normal load (09:01 to 09:25, 130 to 390 tx/s): lag median 44 DAA (4 s), p95 92, max 167 (44 indexer samples); `bytes_per_tx` 1,520 to
  1,930 (small blocks). The flood did not return during the watch, so the flooded behaviour is the catch-up above.

### Lag tolerance in the soak (`max_lag_secs` 30, bots' `maxLagSecs` 30)

The executors' matcher, keepers and maintenance now plan while `caught_up` or within 30 s (300 DAA) of the node, and the bots' gate trades
under the same bound (`judgeHealth(h, error, maxLagDaa)`: `following` or `catching_up` with a known lag at most the bound; `starting`,
`node_unavailable`, `gap`, an unknown lag and a failed request stay closed). `/v1/health` shows `within_lag_tolerance` (true in every
sample since the second start), `caught_up` stays strict.

How much trading it recovers:

* **Under the current load** the follower still flips to `catching_up` when a batch leaves it more than 100 DAA behind: the strict gate
  was closed in **2 of 44** minute samples (4.5 %, lag 116 and 126 DAA), the tolerance gate in 0. Executor logs of 08:55 to 09:01 (before
  the second binary): 2 `waiting for the indexer` episodes on exec-b (123 DAA 14 s, 132 DAA 4 s) and 1 on exec-a (171 DAA, 6 s); after
  09:01 the only waits are the two restarts (6 s each). So about 1 to 4 % of the time.
* **Moderate flood** (the 07:00 to 07:10 stretch of the previous run, `samples.jsonl`): the lag was at most 300 DAA for at most about 3 of
  the 48 minutes the gate was closed (07:01:57 to 07:04 and 07:10:54 to 07:12).
* **Real flood** (2,500 to 7,600 tx/s from 07:11): none; the lag grows by about 400 DAA a minute and passes 300 within a minute of the
  flood's start. The tolerance lets a market ride a short burst, it cannot trade through a flood the indexer cannot follow.
* A book up to 30 s old costs lost races (`MissingInput` / `DoubleSpend`), never a wrong fill (the covenants enforce the terms). The bots'
  conflicts after a long pause are the old restart artefact: 17 amend conflicts at 08:55 and 97 in the first minutes after the 09:01
  restart (the market maker's amends racing its own earlier ones that were still in the mempool).

### Soak fixes

1. **The checker's IOC / FOK deadline.** The 10-02 notes above blamed the node's DAA; the checker was already measuring against the
   indexer's `cursor_daa` (since 4a8763a). The 07:17 incident was real in that measure: the cursor was 1,231 past the kill time, because
   the matcher and keepers **plan nothing while the book lags the node**, so nobody could kill the order. `checkKill(order, cursorDaa,
   grace, followingSince)` now runs the grace from `max(kill, followingSince, born)`: `followingSince` is the cursor DAA at which the
   indexer last started to follow (within the tolerance, `within_lag_tolerance`; null, and no check, while it lags or is down), `born` the
   DAA of the order's current UTXO (an order cannot be overdue before it exists). Two tests in `test/rules.test.ts`.
   Two new `ioc-fok` incidents at 09:08 and 09:10 (KobCross FOK `aafdb055`, KobAsk FOK `8f825e66`) were **mempool latency, not keepers**:
   `aafdb055` was created in a block 1,426 DAA after its own expiry and killed 330 DAA later (the `born` rule clears it), `8f825e66` was
   created 221 DAA before its expiry and filled by a matcher transaction accepted 2,462 DAA (4 min) after the creation. The node's mempool
   held 44,000 transactions and its fee estimate was 140 to 194 sompi per gram for the normal buckets and 115 for the low one, against
   the 100 (the relay floor) the bots and executors pay: at the floor a transaction waits minutes. A third incident at 09:35 (IOC KobBid
   `8fbbf47f`, kill time 586,014,199) is the keeper's own kill transaction accepted 1,523 DAA (2.5 min) after the kill time, on the new
   rules. A higher `fee_rate` would fix the latency (not done: it changes every fee statistic of the soak).
2. **The bank's `Mass calculation error` (80 in one run, 1 in the last).** It is the SDK generator, not the kob-wasm mass call (that is the
   independent `kob.masses` in `recordTx`, which never failed). Root cause, reproduced on the live bank's 90,000 coins and in a unit test:
   the generator takes inputs in order until they cover the outputs plus the fee, and the change is what is left; KIP-9 storage mass is
   `1e12 * (sum 1/output - |inputs| / mean(inputs))`, so a change output of a few hundredths of a KAS costs about 100,000 mass, the standard
   limit, and the generator answers `Mass calculation error` (a little lower, `Storage mass exceeds maximum`) instead of adding an input
   or folding the change into the fee. The bank pays whole 100 KAS chunks from 3.09 KAS coinbase coins, so the residue is fixed for a
   coin set and every 30 s tick failed identically until the set changed (05:25 to 06:05). Fix (`src/bank-plan.ts`): on a mass error the
   request is re-planned (first output +1, +2, +3, +5, +8 KAS, then with fewer outputs; unfunded alternatives skipped; other errors thrown at
   once; the plan's own error if all fail); `bank_mass_retries` counts them. Tests: the pure ladder, and the real SDK generator on 33 coins
   of 3.0867 KAS leaving 0.05 KAS (refused as `Mass calculation error`, paid by the ladder) and 0.2 KAS (accepted as planned). No live
   occurrence since the deploy, so the live proof is the test on the real generator.
3. **Found on the way: the bank never consolidated.** `createTransactions` with no outputs spends ONE input back to the bank (mass 1,624,
   fee 162,400), so the "consolidations" were no-ops: the bank held 90,000 coins (`getUtxosByAddresses` of it timed out at times), and a tick
   re-sent the same transaction while the previous one still sat in the mempool (30 `Rejected transaction` failures in 25 min, all counted
   in `bots failed`). Now the request is one output of the total less 1 KAS: 80 inputs in one transaction (fee 0.09 KAS), one transaction
   per tick (`bank paid out ... consolidated 80`); and the inputs and payees of every submitted transaction are held back for 5 min
   (`RecentSpends`: no identical resend, no double top-up while a payout is unconfirmed). Several ticks since the restart without a
   failure; `node --test` 111 passed, `tsc` clean.

Other observations of the window: 18 `health` incidents in total (all the catch-up, the last at 08:49), no `custody`, `stray`, `fee`,
`x402` or `indexers-agree` incident, 0 rejected and 0 ERROR lines on both executors. `bots failed` (124 at 09:24) is mostly `The node
already has this transaction` (market-maker cancels and token merges resent while the first copy was still in the mempool) plus 2 dropped
connections; x402 settlements waited in the same mempool (`payment_pending`).

### Follow-ups (branch c14/fee, not yet rolled into the soak)

Two findings of this window, fixed on `c14/fee`:

1. **Dynamic priority fee** (soak fix 1 above: the node estimated 140 to 194 sompi per gram while everything KOB built paid
   100). Every KOB transaction now pays the node's `getFeeEstimate` bucket for its urgency, clamped to a configurable cap,
   with the 100 floor when the node gives no estimate: high (priority bucket) for matcher batches that fill an IOC / FOK /
   market / streaming order or a triggered stop, keeper kills, x402 intent executions, and the bots' IOC / FOK / market /
   marketable placements and x402 payments; normal (first normal bucket) for other batches, refunds, intent expiries, resting
   placements, amends, cancels; low (first low bucket) for maintenance, closes, sweeps, the bots' merges and fan-outs and the
   bank. Executor: on by default (`--no-fee-estimate` restores the floor), `--fee-max-rate` 1000 sompi/g, `--fee-max-tx-kas`
   1; a batch is planned at the normal rate and an urgent one only goes at the high rate while it stays profitable there
   (`docs/ops/executor.md`, "Fee policy"; metrics `kob_fee_rate_*`, `kob_fee_estimated`; `submitted` log lines carry `fee`
   and `fee_rate`). No RBF: a stuck transaction is not replaced (documented in `docs/spec/matcher.md` §7). Bots and checker:
   `"fees": {"dynamic": true, "maxRate": 1000, "maxFeeKas": 1}` in `run/config.json` (an absent key means the same defaults;
   `"dynamic": false` restores the floor); `txs.jsonl` lines gain `urgency`, `feeSource`, `feeReason`, `bucketFeerate`,
   `cappedFrom`, `overCap`; the checker's fee rule is now `fee == feeRate x max(compute, transientNormalized)` with the
   recorded rate in [100, `fees.maxRate`] (old lines at 100 stay valid). Every fee statistic of the soak (`fee-compare.mjs`,
   `bot fees`) rises under load by design.
2. **Prefetch memory bound is hard** (the 341 / 327 MB peaks against the 268 MB budget). A window in flight now reserves an
   allowance that is also its request's size limit (the websocket client refuses a larger answer as its frame header
   arrives); an oversized window raises the per-block estimate at once and is split. `prefetch_oversize_total` counts them
   (`/v1/health`, metrics). Default `prefetch_max_mb` 256 -> **128** (RSS is about 3x the budget at the peak).

Roll-in (after the merge; same keys, databases carry over, no template change):

* executor `--features deploy-tn10` (private target dir, `CARGO_BUILD_JOBS=1`), `scripts/stop.ps1`, previous binary and
  `run/config.json` to `run/backup-pre-<sha>/`, copy the binary;
* `run/config.json`: `"prefetchMaxMb": 128` (it writes `prefetch_max_mb` explicitly, so the new default alone does not
  apply), and `"fees": {"dynamic": true, "maxRate": 1000, "maxFeeKas": 1}`; the executors need no flag for the fee policy
  (`"args": ["--fee-max-rate", "..."]` per executor only to change the defaults);
* web `build:wasm` is not needed (kob-wasm unchanged); rebuild the soak bundle (`dist/soak.mjs`) and `build-ui.sh` (the
  web wallet discloses the fee rate now); `scripts/start.ps1`;
* check: `fee rates (sompi per gram)` lines in both executor logs (`estimated=true` while the node answers),
  `kob_fee_estimated 1`, `txs.jsonl` lines with `feeSource: "estimate"`, no `fee` incidents in the checker, and
  `prefetch_bytes_peak` at most 134,217,728 at the next catch-up.

## Redeploy on main 5a3e9a3 (2026-10-02, TN10): dynamic priority fee, hard prefetch bound

`m9/soak` merged `main` 5a3e9a3 (fast-forward; no template change, so databases, tokens and orders carried over). Roll-in as in the
follow-ups above: executor `--features deploy-tn10` (private target dir, 2 jobs, 2 m 04 s incremental), the PC had been rebooted and the
soak stopped cleanly beforehand; previous binary and `config.json` in `run/backup-pre-5a3e9a3/`; `run/config.json` got
`"prefetchMaxMb": 128` and `"fees": {"dynamic": true, "maxRate": 1000, "maxFeeKas": 1}`; `npm run build`, `node --test` (126 passed),
`scripts/build-ui.sh`; `scripts/start.ps1` at **11:53:10 UTC**. Both executors resumed their databases and were within the lag tolerance
(lag 1 to 2 s) within a minute; the bots' gate resumed trading at once (no long outage to catch up: the stop was clean).

Watch 11:53 to 12:38 (45 min, samples every ~5 min):

* `fee rates (sompi per gram) high=100 normal=100 low=100 floor=100 estimated=true` in both executor logs, `kob_fee_estimated 1`,
  `kob_fee_estimate_failures_total 0` for the whole window. The TN10 mempool was calm (the estimate equals the floor), so the executors
  paid 100 in every bucket: all 130 `submitted` lines carry `fee_rate=100`. The estimate path is proven live, the raise under load is not
  (not exercised in this window; unit tests only).
* Bots (`txs.jsonl` since the start, 892 txs, 13.08 KAS in fees, all `feeSource: "estimate"`, 0 `overCap`, 0 capped): normal/100: 634,
  high/100: 102, low/100: 39, plus brief node-estimate moves picked up per transaction: 11:56 normal 148 (bucket 147.2) and high 191
  (190.0), 12:26 normal 177 (176.3) and high 335 (334.4). The rate is the bucket rounded up; never above the cap.
* `prefetch_bytes_peak` 134,217,728 on both indexers (exactly the bound, not above); `prefetch_oversize_total` 1 (exec-a) and 22
  (exec-b, window 64 blocks after the splits): oversized windows are split as designed. The restart catch-up was short, so a long
  catch-up under the 128 MB budget is still to be seen.
* Latency: no `ioc-fok` (or any other) incident since the restart, 769 IOC/FOK checks passed by the checker; no kill or fill was late
  at the floor rate because the mempool was empty. The 2 to 4 min lateness of the last round (44,000-tx mempool, estimate 140 to 194)
  could not be re-measured in calm conditions.
* Incidents: 61 in total, none new since the restart (the latest, `health`, at 10:57 was the earlier catch-up). Rejected: 0 on both
  executors; bots txs ok 794, conflict 27, failed 0 at 12:38.
* Memory (working set, MB, now/peak): exec-a 89/89, exec-b 70/75, bots 308/423, checker 126/136, ui 46, miner 16; total about 655
  (peak 769). The executors stay under 100 MB, against 341 / 327 MB peaks before.

The soak was left running.

## Third token TBTC, USD charts, near-zero spreads (2026-10-03, TN10, m9/soak 50932af + c37ea7b)

New token only, no wind-down: `scripts/stop.ps1`, `soak.mjs setup` (issues the new `token3` slot, rewrites the registry = the executors'
allowlist), `scripts/start.ps1` at **02:06 UTC**; the public `--public` UI instance restarted by hand. Backup of the previous
`config.json`, `state.json`, registry and web build in `run/backup-pre-tbtc/`.

* **TBTC** (`Test BTC (tBTC)`, covenant `78328594…`, KCC20Ref_8x8, 50 supply, 8 decimals, lot 0.000002 TBTC ≈ 4 KAS, tick 10,000 sompi per
  lot), issued by KOB's issuance (tx `31d82684…`): 30 % to the market maker, 8 % to each trader, the rest to the bank. Reference Binance
  `BTCUSDT / KASUSDT`. The soak code now handles the tokens beside TUSD as slots (`ASSET_SLOTS` = `token2`, `token3`: config, state
  `tokenN` / `genesisOutputsN`, registry, market maker per book, traders with per-token `bookShare` / `crossShare` (TETH and TBTC 0.22 /
  0.15 each, TUSD the rest), cross limits and pair market orders TBTC↔TUSD, fan-out, consolidation, checker invariants and supply, report,
  balances, holdings). The checker's supply line now reads `TUSD … TETH delta 0  TBTC delta 0`.
* **Near-zero spreads.** `mm.innerBps` 25 -> 2, `stepBps` 30 -> 4, re-quote per level (`requoteMinBps` 3, its own distance from the
  mid, at most `requoteBps` 20), the side the reference moves away from first, inventory skew capped at `maxSkewBps` (8 at the start,
  3 since 02:46: the 8 bps cap on two books showed up as a constant bias in the implied prices, e.g. +18 bps on BTC/USD = +8 TBTC −
  (−8) TUSD). Ticks are far below a bp (TUSD 0.04 ppm, TETH 0.16 bps, TBTC 0.25 bps), so the 2 bps first level fits every book.
* **Reference record.** The bots append every good Binance poll to `run/state/ref-prices.jsonl` (`{t, s, p}`, exchange time, ~3 s apart
  per symbol) and write `run/state/ref-symbols.json`; `scripts/tracking.mjs` compares the soak's USD prices against it. There is no
  reference line in the UI (founder 2026-10-03).
* **USD charts** (web `charts/usd`, cherry-picked onto m9/soak as c37ea7b: main is ahead of the soak by template changes, so a merge of
  main would need a wind-down): serve-ui sets `quoteTokens` (TUSD = USD); the landing screen is KAS/USD, `#/usd/<TETH>` and
  `#/usd/<TBTC>` are ETH/USD and BTC/USD, the market list has a "USD prices" table. Screenshots: `node scripts/screenshots.mjs --only usd` (local, and `--url <public URL> --tag public`
  through the tunnel; the earlier set showed the removed language switch and was dropped, regenerate from the live soak).

Watch 02:10 to 03:24 UTC (spreads sampled every 30 s from `/v1/depth` levels with an amount, 02:46 to 03:24):

| | |
|---|---|
| KAS books, best ask − best bid | median TUSD 4.1, TETH 3.6, TBTC 4.0 bps (design 2 + 2); p90 20 / 13 / 7 bps; 16 % of samples crossed for a moment (a trader's limit through the touch until a matcher fills it, never longer than a few ticks) |
| implied pair books (route) | TETH/TUSD median 18 bps (route levels on both sides in 5 of 76 samples), TBTC/TUSD 7 bps (15 of 76) |
| tracking, trades (02:48 to 03:24) | KAS/USD median 8.0 / p95 30 bps (n 84), ETH/USD 11.9 / 56 (n 45), BTC/USD 17.7 / 76 (n 47); bias +4.8 / +3.2 / +11.8 bps |
| tracking, the app's 1m candles | KAS/USD 8.3 / 28, ETH/USD 10.5 / 45, BTC/USD 14.9 / 59 bps |
| tracking 02:10 to 02:45 (skew cap 8) | KAS/USD 7.4 / 21 (bias +7.6), ETH/USD 12.3 / 35 (+8.3), BTC/USD 19.0 / 39 (+18.3) |
| matcher | 231 match transactions in 1.23 h (187 per hour against 151 before), 261.5 KAS profit after fees (212 KAS per hour against 146), median 0.29 KAS per match, none negative: tight quotes leave the margin to crossing traders, market auctions and cross-limit routes and tips, which still pay the fee of every fill |
| TBTC market | 299 fills / 102 trades in 1 h 17 min, cross limits and pair market orders in both directions (TUSD>TBTC, TBTC>TUSD) |
| invariants | 0 incidents since the roll-in (all invariants incl. 11 cross and 12 supply over the three tokens: TBTC supply delta 0 every round) |

The deviation is mostly where trades happen inside the ladder (2 to 22 bps from the reference) plus the traders' crossing orders, not a
lag: the best lag per market (0 to 600 s scan) improves the median by 1 to 4 bps only. The soak was left running.

## Redeploy on main 747797c (2026-10-03, TN10): payment-intent router re-pinned, bid amends in place, final watch and fault injection

`m9/soak` fast-forwarded to main 747797c (later a3b003e and 92b5cc3, web only: unified market page, silent ordinary reorgs, pair
depth / trades / stats). **Templates:** `templates()` of the soak's kob-wasm and of main's are identical (all 22 entries: the order
covenants, their KRON twins, the token programs); none of them is in main's 36 `retiredTemplates()`. Only the **x402 payment-intent
router** changed: `cf3ad2f8` -> `cd1a2677` (B3: per-intent program, 18 actors re-pinned plus 12 KRON actors). Databases, tokens, keys
and all live orders (KobCross 99, KobBid 21, KobAsk 21, KobCondBid 16, KobCondAsk 7, if-done 3, all on unchanged hashes) carried over.

Wind-down (old binaries): 06:40 UTC `x402.invoiceIntent.enabled = false` and a bots-only restart (the facilitator kept intents on to
execute or expire what was open); the last invoice expired at 06:44; every intent of the run (132 in the bots' logs) has no unspent
token UTXO (`/v1/token-utxos?owner=<intent covenant id>`) and the facilitator ledger held no pending entry. Builds (private target dir,
2 jobs): executor `--features deploy-tn10` 2 m 43 s, `build-wasm.sh --tn10 --web` 3 m 02 s, web `build:wasm` (slim) 1 m 05 s, soak bundle
(typecheck, 129 tests), `build-ui.sh` now pinning the soak registry (sha256 `5985081b…`: no custom-registry dot or labels; the tokens
read `[unverified]` and `genesis not verified` because the soak registry has no genesis record and only `official` entries get an
indexer standing other than `unverified`). Miner unchanged (no commit since the GPU build). Previous binaries, bindings, web build,
bundle, config and state in `run/backup-pre-747797c/`. `scripts/stop.ps1`, copy, `invoiceIntent` back on, `scripts/start.ps1` at
**06:49:07** (both indexers within the lag tolerance at 06:49:20; `x402: intent-based swap-and-pay on (router cd1a2677…)`; the new
`order_amends` columns are added in place). The public instance (`serve-ui.mjs --config %TEMP%/ui-pub.json`, now generated from
`run/config.json` with `ui.listen` 127.0.0.1:8491 and `public: true`) was restarted; the cloudflared quick tunnel kept its URL.

### Watch 06:49:20 to 09:50 UTC (3 h, minute samples `run/watch-747797c/samples.jsonl`, `analysis-watch.json`)

| | |
|---|---|
| indexers | `following` and within the tolerance in all 181 samples on both; lag median 18 / 19 DAA, p95 28, max 40 / 36; 0 VSPC timeouts |
| matchers | exec-a 327 and exec-b 360 batches finalized (570 match, 120 refund / kill), **0 rejected**, 0 unknown submits, 1,233 lost races (`MissingInput` / `DoubleSpend`); profit 280.5 + 304.1 KAS; stops: 36 trail updates, 8 triggers; 0 WARN / ERROR lines |
| dynamic fee | estimate path live (`kob_fee_estimated 1`, 0 failures); the TN10 mempool stayed calm, so every bucket was 100 in every sample and all 5,494 bot transactions paid 100 (normal 4,582, high 607, low 305; 0 capped) |
| bots | 5,494 transactions, 74.81 KAS fees (0.0136 KAS per tx), every order type of the mix (market, GTC / GTD / day / timed, IOC, FOK, streaming, stop-market / -limit, trailing, take-profit, OCO, IFD, IFO, IFO stop entry, repeat IFD / IFO, TWAP, DCA, Dutch, close, cancel, cancel-all), 263 cross limits, 42 cross FOK, 134 pair market orders, 305 consolidations |
| amends in place | **bids 1,161 on 475 orders** (B1: one `amend` event per submission, same covenant id), asks 1,107 on 420 orders |
| x402 | native 22, KCC-20 22, swap 14, **invoice + intent 21 on the new router** (5 to 9 s); facilitator: 86 settle requests, 79 success, 7 `order_conflict` on `/swap` (the quoted order filled first; the payer re-quotes on its next turn), 26 intent executions, 5 intent conflicts, 0 intent failures |
| tracking vs Binance (trades, then 1m candles: median / p95) | KAS/USD 7.7 / 25.3 bps (n 428), 7.4 / 26.6; ETH/USD 13.3 / 37.4 (n 253), 10.8 / 39.8; BTC/USD 12.5 / 41.2 (n 214), 9.9 / 39.0; bias +4.5 / +5.8 / +6.8 bps |
| invariants | **1 incident**: `trigger` **warn** at 09:13 (IfdBid `caaddf9f` armed in tx `a43e1412`; one evidence candidate, a plain bid, has no slope / exposure in the indexer data, so the checker cannot prove it: the documented unverifiable class, not an error). Supply TETH / TBTC delta 0 every round, TUSD at its baseline |
| resources (working set, MB, median / max) | total 916 / 1,024: exec-a 166 / 203, exec-b 95 / 105, bots 219 / 233, checker 141 / 186, ui 62 / 74, miner 240 (the OpenCL GPU miner, idle: the bank is above the high watermark) |

`bank tick failed: Storage mass exceeds maximum` 56 times in 274 bank ticks: the ladder of `src/bank-plan.ts` finds no plan for some
coin sets (no wallet ran short; payouts and consolidations went through on the other ticks). Open soak-side item.

### Fault injection (09:51 to 10:12 UTC, one at a time; `run/watch-747797c/analysis-faults.json`)

| fault | what was done | outcome |
|---|---|---|
| executor killed mid-batch | 09:51:25.68 `taskkill` of exec-a 0.1 s after `submitted txid=fb6fba82… kind=match` | restarted 09:51:27.8, planning again 09:51:36; the batch was accepted (its fills are in the indexer); the bots' gate paused 10 s; 0 rejected |
| indexer down ~10 min | 09:51:43 to 10:01:43 every restart of exec-a killed (12 kills, each start lived at most ~10 s), last backoff start 10:02:15 | within the tolerance at 10:03:01 (46 s for 10.5 min of chain: prefetch at its 128 MiB bound, 6 oversize windows split, 0 VSPC timeouts); bots paused 09:51:44 to 10:03:02 (678 s) and resumed at `catching_up` lag 72 DAA; x402 down with exec-a; 1 `health` error incident (exec-a unreachable, expected) |
| node connection lost | no firewall rule (the shell is not elevated): exec-b's node URL pointed at a local TCP relay (config edit + `restart-child.ps1 -Name exec-b`, config restored at once); the relay dropped every connection and refused new ones 10:04:36 to 10:06:37 | exec-b stayed up: transport errors, failed ticks, `node_unavailable`; reconnected at 10:06:37, planning at 10:06:58; exec-a and the bots unaffected; exec-b back on the direct URL at 10:07:14 |
| miner restart | 10:07:19 `restart-child.ps1 -Name miner` | up at 10:07:25 on the GPU (RTX 3060, OpenCL), idle above the watermarks as before |
| checker killed | 10:10:46 `taskkill` | restarted 10:10:48, resumed at round 1908 from its store (no reset, no duplicate incident) |

After the faults: no `ioc-fok`, `custody`, `stray`, `cross`, `supply` or `indexers-agree` incident, 0 rejected on both executors, the
books two-sided again within seconds of each resume (status at 10:16: TUSD 7 asks / 6 bids, TETH 7 / 6, TBTC both sides), supply deltas
unchanged.

### Screenshots

`scripts/screenshots.mjs` is rewritten for the unified market page: `screenshots/202610031013-*` (local) and `202610031015-public-*`
(through the tunnel; the public TUSD page retaken at 10:19, its book had been crossed): the market list, every token's market page and
its flipped view, TUSD light / 1280 px / phone, the TETH/TUSD and TBTC/TUSD pair pages (light and phone for TETH/TUSD), My orders of
trader t1 and the confirmation of an unsigned 1-lot limit buy (the web app's test wallet with `approve: false`, injected only into the
capturing browser; those two pages show the test-mode banner).

Observed in the UI (left for the web branch): a soak token reads `[verified]` on My orders but `[unverified]` on the market list and
page (the order rows label without the indexer standing, the market pages with it).

The soak was left running.

### Follow-ups of the final soak (10-03)

* **Bank "Storage mass exceeds maximum"** (56 of 274 ticks): the bank held one merged coin of 461,621 KAS and 79 small ones, and the
  consolidation (output = total - 1 KAS) stopped on a 79-coin prefix whose change (~0.001 KAS) is in the storage-mass dead band; the output
  ladder only raised the output, which shrinks the leftover, so every tick failed identically until a coinbase coin arrived. `bank-plan.ts`
  now tries, right after the plan itself, a coin set from `marginCoins` (only the coins the request needs plus a margin of 0.4 KAS and the
  dearest fee, the largest coin last: no shorter prefix can cover the outputs, so the change is the margin, never a dead-band residue), then
  the old output ladder with the default and the margin coins. Tests reproduce the live coin set (the plan refused, the old ladder refused,
  the margin coins pay), sweep every keep from 0.2 to 6 KAS and every payout of 1 to 12 chunks on it, and 90 seeded random coin sets.
* **Same token `[verified]` on My orders, `[unverified]` on the market** (web branch `fix/two`): the market passed the executor's standing
  into the label and badges, My orders and balances did not, and the executor says `unverified` of every token that is not `official`.
  `indexerDowngrade` (web/src/ui/market/token-model.ts) is now the one rule: `delisted` always wins, `unverified` only against a registry
  token that claims `official`, `official` never upgrades. The soak tokens (verified, not official) read `[verified]` everywhere.

## Redeploy on main 8c7459d (2026-10-07, TN10): KobIfdAsk refund fix, token-intent lock pin, executor transport bounds

`m10/soak` fast-forwarded from 217d452 to main 8c7459d (10 commits). **Templates:** of the 15 order templates the soak pinned, only
`KobIfdAsk` 189b9c32 -> bbcfc226 and `KobIfdAskKron` 85d87838 -> d523d794 changed; both old hashes are in `kob_protocol::retired` with
today's state layout (test `every_order_template_of_the_tn10_soak_build_is_pinned_or_retired`). The 24 token-intent router templates
changed too (router a85ee4b6 -> f50258c3). **Databases kept** (schema 5 unchanged): every candle (1m / 5m / 1h / 1d, KAS books and
pairs) that closed before the switch and the 200 newest trades per book read back byte for byte from both indexers afterwards.

Procedure: `invoiceIntent` off and a bots restart, until the facilitator ledger held no open intent and the last invoice expired
(~6 min); bots held down (`dist/soak.mjs` renamed away, so the supervisor's restarts fail) while the 3 live `KobIfdAsk` 189b9c32
entries were cancelled through the retired path of the new `kob order cancel --view` (both old indexers recorded `cancelled`; 0 live
orders on a changed template); executors swapped one at a time (`executorBin`, `restart-child.ps1`); `run/wasm-node`, `dist/`,
`run/web-dist` (built with `UI_OUT` into a staging directory) swapped; bots, checker, UI and the public 8491 instance restarted.
Bots were down 22:06:45 to 22:18:52 UTC. Previous binaries, bindings, bundle, web build, config and database snapshots
(`VACUUM INTO`) in `run/backup-pre-8c7459d/`.

Found on the way (fixed, regression tests):

* **The facilitator ledger refused to open** (`ledger corrupt at line 49: missing field lockAmount`): every intent payment recorded
  before the lock pin failed to decode, so the new exec-a exited at start (exec-a was rolled back to the old binary for 8 min). Such a
  record now replays as `legacyIntent` (kept verbatim, never acted on); 123 of the 488 entries.
  (Superseded: earlier intent formats are no longer read; such a ledger stops the facilitator at start with a message to
  archive it, see `docs/ops/executor.md` A.6 "Ledger format".)
* **Orders of a retired template with today's layout were listed**: a carried database keeps them under the contract name, their state
  decodes as today's kind, and a fill / kill / refund planned from today's template cannot spend their script. The book now offers only
  orders whose recorded template hash is pinned.

Watch 22:20 to 22:52 UTC (32 minute samples): 0 rejected (exec-a 85, exec-b 90 batches finalized), 0 new incidents (the two `health`
errors at 22:10 / 22:12 are the swap), 0 rejects in the indexer; fills per minute (22:22 to 22:32) TUSD 5.5, TETH 9.1, TBTC 4.6, pair
fills TETH/TUSD 2.7, TBTC/TUSD 1.4; IOC / FOK / market orders, stops, trailing stops, pair market and pair stops, and the new
`KobIfdAsk` bbcfc226 entries (armed and filled) on TETH and TBTC all filled; the invoice + intent path paid on the new router. Indexer
lag median 48 to 55 DAA (p95 120 to 230): TN10 carried ~240 transactions per block in this window (8 MB batches, 20 s fetches), so both
indexers touched `catching_up` 3 to 4 times, as before the switch. Bank 295,656 KAS, falling ~3,000 to 3,800 KAS/h (market-maker
top-ups); it reaches the miner's 20,000 KAS watermark around 10-10 / 10-11, after which the GPU miner (~1 block/s while bursting) holds it.

## TN10 flood 10-07: Borsh windows (main 854b079, d4a2cc5, 414059c)

From about 23:00 UTC on 10-06 TN10 carried 2,200 to 2,700 plain 1-in/1-out transfers per second. Both indexers fell to 19-27k DAA
behind (`catching_up`) because the soak PC's link tops out at about 7.6-8 MB/s, and the JSON VSPC windows cost about 1,420 bytes per
transaction, which is 5-7 MB/s per indexer just to keep pace. No amount of connections or prefetch budget could help.

* `--borsh` (main 854b079) fetches the windows and the primary's ids over Borsh wRPC: about 575 bytes per transaction (`High`),
  2.5-2.9x fewer bytes. Both executors run it (`executorBin` `bin/kob-executor-minlag1.exe`, `--features deploy-tn10`). After the
  switch each indexer ingests 4,400-6,300 tx/s, about 2x the chain; exec-a went from 27,240 to 250 DAA behind in about 50 min and
  exec-b from 19,000 in about 35 min (exec-b was throttled to `fetchParallel` 2 while exec-a caught up).
* `--prefetch-min-lag-blue 30` (d4a2cc5): with the default 1,200, the last two minutes were single steps on one connection
  (~1.2 MB/s against ~1.7 MB/s of chain), so the lag settled at 550-950 DAA.
* exec-b no longer has `--no-primary-fetch`: at times the two public nodes gave exec-b only 50-90 KB/s per window.
* `maxLagSecs` 120 while the flood lasts. The bots' gate opens on `lag_daa <= maxLagDaa` in `catching_up` too.
* Bot reservations last 10 min, and a mempool double-spend refusal reserves the inputs for 1 min (414059c).

Watch 01:52 to 02:07 UTC, with the flood still running: lag median 397 / 314 DAA (exec-a / exec-b), p90 730 / 848, max 1,038 /
1,204; 0 rejects, 0 lies. Fills per minute: TUSD 0.8, TETH 0.5, TBTC 1.7. Amend and market-maker conflicts are still high (72 / 155),
because the bots plan on a book 30-80 s old. The remaining limit is the link: both indexers together need about 3.5 MB/s at the tip,
and the transport waste (refused oversize windows, TLS) brings that to about 7.6 MB/s on the NIC. Following within 5 s through such
a flood needs the node on the same host or LAN (docs/ops/executor.md, *Bandwidth*).

## Redeploy on main 1c51b27 (2026-10-08, TN10): the public history of 10-07 / 10-08, unpinned templates dropped

`m10/soak` fast-forwarded from 4534e0f to main 1c51b27 (the 81 public commits after 7e216a7, the public twin of 4534e0f, cherry-picked;
the tree equals public master 2f3fd75) plus two soak fixes. **Templates:** 11 of the 15 order templates changed (`KobAsk`, `KobPair`,
`KobCondAsk`, `KobCondBid`, `KobCondPair`, `KobIfdAsk`, `KobIfdBid`, `KobIfdPair`, `KobCondBidKron`, `KobIfdBidKron`,
`KobIfdAskKron`; `KobBid`, `KobAskKron`, `KobBidKron`, `KobCondAskKron` kept) and the token-intent router. This build has no
`kob_protocol::retired` any more (a9e2636: an order of a template the build does not pin is never decoded or listed), so every live
order was ended with the OLD build first. **Databases kept** (schema 5 unchanged): a dry run of the new indexer on a `VACUUM INTO` copy
of exec-b's database served the same history, and after the switch every candle (1m / 5m / 1h / 1d of the three KAS books and the two
pairs) that closed before the cutoff and the 1,000 newest trades and pair fills per series read back identical from both indexers
(24,134 items in 50 series, `confirmations` / `settled` aside).

Procedure: `invoiceIntent` off and a bots restart (no open intent in the facilitator ledger, the last invoice expired 19:48 UTC); bots
held down (`dist/soak.mjs` renamed to `soak-old.mjs`) and both matchers paused; the 109 live orders of both indexers (KAS-book asks /
bids, stops, if-done, pair, conditional pair and if-done pair orders) cancelled with `soak-old.mjs cancel` (the old bundle and
bindings), until both indexers listed 0 open / partial / active orders and both books and pair books were empty (19:55:48 UTC).
Balances before and after the cancels, database snapshots, the old bundle, bindings, web build, config and the history snapshot are in
`run/backup-pre-1c51b27/`. Executors swapped one at a time (`executorBin` `bin/kob-executor-1c51b27.exe`, same `--borsh`, two
`--node`s, `--prefetch-min-lag-blue 30`, `maxLagSecs` 120); `run/wasm-node`, `dist/`, `run/web-dist` (built with `UI_OUT` into
`run/stage-1c51b27/`) swapped; bots, checker, UI and the public 8491 instance restarted (the tunnel kept running). Bots were down
19:52:12 to 19:57:27 UTC.

* **The facilitator ledger is archived, not carried:** this build refuses a ledger with intent records of the format before the lock
  pin (`legacyIntent`, 123 entries), so `x402-ledger.jsonl` and `x402-invoices.jsonl` moved to the backup and the facilitator started
  an empty ledger; the payer's `state/x402-payments.jsonl` went with them. The checker had read the payer's record against the new
  ledger once (19:57:31, 726 `x402` incidents "paid but not in the facilitator ledger", all payments before the switch).
* **Every `/token` payment was refused** (`spend_not_authorized`): the SDK now holds a payment's carrier and network fee to the
  payer's KAS ceiling, and a token payment's 1 KAS carrier plus fee exceeded the soak's 1 KAS. The ceiling now covers the carrier
  (or the native price) plus a fee at the payer's cap (`payerMaxAmount`, test in `x402-caps.test.ts`); `/token` paid again at 20:23.
* The soak's typecheck follows `web/src/app/services.ts` into `import.meta.env`: a vite env shim in `src/shims.d.ts`.

Watch 19:58:48 to 20:18:48 UTC (20 min): 0 rejected (exec-a 45 submitted / 44 finalized, exec-b 99 / 97), 0 rejects and 0 lies in
the indexers, both `following` (lag 8 to 9 DAA at the end); two `health` incidents (each indexer touched `catching_up` once,
19:59 / 20:00). Fills per minute: TUSD 7.1, TETH 8.1, TBTC 8.15; pair fills TETH/TUSD 2.05, TBTC/TUSD 2.3 (routed). Orders on the
new templates (`KobAsk` 070bb3b2, `KobPair` c95c9234, `KobCondPair` 7b8f1a9e, `KobIfdPair` 4a432afb) placed and filled; the invoice
+ intent path paid on the new router. `/swap` fails with `order_conflict` as before the switch (about half of the attempts on 10-07:
the market maker amends the bids it takes faster than one quote-to-settle round). The public page (tunnel) shows the carried chart
history (TUSD/KAS 5m from 10-06, 24h stats) and the recent trades from before the cutoff.
