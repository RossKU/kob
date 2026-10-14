# KOB TN10 soak

A live testnet-10 market that looks and behaves like a real one. Three test KCC-20 tokens of KOB's standard program (`KCC20Ref`:
the reference with 3 token inputs / 3 token outputs per transaction; fixed supply, 8 decimals): **TUSD**, whose KAS price follows
**Binance KAS/USDT** (1 TUSD ≈ 1 USD worth of KAS), **TETH** and **TBTC** (1 ≈ 1 ETH / 1 BTC worth of KAS, Binance ETH/USDT and
BTC/USDT over KAS/USDT), each with its own KAS book, and the TETH/TUSD and TBTC/TUSD pairs. A market maker quotes every KAS book,
five traders place every KOB order type in the KAS books and as pair orders, a payer makes x402 payments (KAS, KCC-20,
swap-and-pay, invoices paid by an intent), two competing `kob-executor run` instances (indexer + matcher + keepers) fill and
maintain the books, an invariant checker reads everything, and the web UI runs on the live data. Everything runs on one Windows PC
against a testnet-10 node of your own (`ws://127.0.0.1:18210` by default; point `config.json` at it); all KAS comes from
self-mining.

Nothing on chain depends on the Binance price: the bots only *read* it to decide where to quote. There is no oracle.

## Units and the checker's invariants

Every quantity (order amounts, `amount_left`, `min_fill`, custody, fills) is token **base units**; every price and tip is sompi per
**whole token**, i.e. per `scale` base units of the order's token (`scale = 10^decimals`, a field of every order; a pair order's
price is token B base units per whole token A). The value of `n` base units at a rate `r` is `n × r / scale`, rounded in the maker's
favour (up for what a maker must receive, down for what a maker pays; `docs/spec/order-types.md`, "Amounts, prices and rounding").
The bots price every book from the Binance reference (`price.ts` `RefPrice.perToken`; setup refuses more than 9 decimals) and draw
every size as any amount of base units: the market maker between `mm.minTokens` and `mm.maxTokens` whole tokens of its book, a
trader between half a dollar and its `maxUsd`, a pair order `pairUsd` dollars of its base token. Minimum fills, trigger thresholds
and every KAS amount (bid escrows, prefunds, carriers) come from the web planners and kob-wasm's exact helpers. The registry the soak
writes has no `lot_size` and no `tick` (`kob_protocol::registry` reads and ignores both).

The checker (`src/checker/run.ts`, formulas in `rules.ts`; incidents in `run/incidents.jsonl`) runs every invariant over every soak token:

| # | Invariant | What it asserts |
|---|---|---|
| 1 | custody | an ask-side order's live custody token UTXO holds exactly `amountLeft` base units and is in the node's UTXO set; a live plain ask or `KobPair` keeps `filled_amount + amount_left == initial_amount` |
| 2 | stray | a stray that disappears from `/v1/strays` was spent by a `cancel` of its order |
| 3 | all-in | a sell receives at least `ceil(n × (q − tip) / scale)`, a buy pays at most `floor(n × (q + tip) / scale)`; a merged repeat take-profit pays the maker the proceeds minus `ceil(n × rptPrice / scale)`; a sell-first entry funds its exit with at least `ceil(n × (q − tip) / scale)` |
| 4 | ioc-fok | no IOC / FOK is live 1,200 DAA past its kill time; a FOK is never partial |
| 5 | trigger | every `arm` / `trail` event is backed by a fill of the stop's token and `scale`, of at least `minTouch` base units, in the same transaction |
| 6 | repeat | entry `amountLeft` + live booked exit `amountLeft` `<= initial_amount`; an exit's `rptPrice` is the entry's `price + tip` (buy-first) or `price − tip` with `rptPre = prefund` (sell-first) |
| 7 | x402 | every payment the payer got a 200 for is in the facilitator ledger exactly once and `accepted` |
| 8 | fee | every bot transaction (`run/state/txs.jsonl`) pays exactly rate × max(compute, normalized transient mass), at a rate in [100, `fees.maxRate`] |
| 9 | indexers-agree | both indexers list the same orders and fills (up to the lower cursor less 200 DAA) and agree on sampled orders' `filled_amount` and `amount_left` |
| 10 | health | follower state, lag and progress of both indexers; node sync, book staleness and step age of both executors |
| 11 | pair | a pair order fill carries no price (volume only) and honours the order's guarantee at its quote; a pair stop arms only from evidence of one of the two modes |
| 12 | supply | per token, the live token UTXOs add up to the supply |
| 13 | scenarios | the bots' scenario records (`run/state/scenarios.jsonl`, `scenario-rules.ts`): a limit ladder's rungs are independent orders of one size and cancelling one rung touches no other (`ladder`); a GTC renewal keeps its terms with a later expiry and a pair amend keeps pair, side and size (`replace`); a cancel-all ends every order (`cancel-all`); a token merge ends in one output of the inputs' total (`merge`); an x402 payment has at most one accepted attempt and a lost answer is replayed (`x402`) |

A checker state file of another protocol generation (`run/state/checker.json` without `"protocol": 3`) is dropped at start.

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
| traders t1..t5 | one action every 45 to 120 s (exponential), weighted over every order type of `docs/spec/order-types.md`: market (auction), limit GTC (passive and crossing), GTD (10 to 40 min: keeper refunds), day, timed activation, IOC, FOK, streaming, stop-market, stop-limit, trailing stop, take-profit, OCO, IFD buy-first / sell-first, IFO, IFO with stop entry, repeat IFD, repeat IFO, TWAP, DCA, Dutch / rising bid, close, cancel, cancel-replace (amend), cancel-all; since 2026-10-13 also a **limit ladder** of 2 to 20 same-size rungs (the ticket's ladder, one order and one transaction per rung), the **cancel of one rung**, the **GTC renewal** (My orders' renew) and **Merge tokens** (see "Scenarios" below). Weights: `DEFAULT_WEIGHTS` in `src/bots/weights.ts`, `traders[].weights` overrides per key |
| pair orders | a share of the trader actions (`token2.pairShare`, 15 %) are **pair orders** of the asset token A (TETH, TBTC) against TUSD B through the web planner (`planOrder` with a PairPlanEnv built by `market.ts` `pairPlanEnv`, the same environment as the app's `buildPairPlanEnv`): KobPair limits on both sides (35 % resting 0.2 .. 1 % passive for 5 to 20 min, the rest crossing by 0.2 .. 1 %: the matcher nets opposite pair orders or routes them through the two KAS books), IOC and market orders, Dutch, TWAP / DCA, FOK, streaming and close, KobCondPair stop-market, stop-limit, trailing stops, take-profit and OCO (armed by the matcher from pair evidence: two KAS-book fills of A and B, or a resting pair order filled at or beyond the stop), KobIfdPair IFD / IFO buy-first and sell-first, IFO stop entries and repeat IFD / IFO: every order type of the KAS ticket (`DEFAULT_PAIR_WEIGHTS` in `src/bots/weights.ts`, `token2.pairWeights` overrides). Since 2026-10-13 also dated pair limits (GTD 30 to 120 min, day, timed activation), the **pair amend** (the pair ticket's replace) and the **pair cancel-all** (see "Scenarios" below). Prices are B base units per whole A around the pair book's touch midpoint (when within 3 % of the Binance-derived fair rate refA / refB, else the fair rate); sizes 3 to 20 USD of A (`pairUsd`). Pair fills are volume only: they never set a price (checker invariant 11). |
| x402 | a paywall with three resources (`/native` 0.5 KAS, `/token` 0.25 TUSD, `/swap` 0.5 KAS paid with TUSD through KOB bids) settled by exec-a's facilitator; the payer pays them in turn every ~2 min. With `x402.invoiceIntent.enabled` every fourth turn is an **invoice paid by an intent** (docs/ops/executor.md A.7): the merchant registers an invoice of `amountKas` (20) KAS whose only entry is an intent swap offer paid with TUSD, the payer signs the router intent's creation (`payInvoiceWithIntent`, at most `maxSellTusd` sold) and the facilitator executes it against the TUSD bids; the supervisor then switches the facilitator's `intents` (keeper = exec-a's key) and `invoices` on. Since 2026-10-13 the payer's turns are drawn by weight (`x402.weights`, default `/native` `/token` `/swap` `invoice` 3 each) and include the SDK's **retry paths** `/retry/drop`, `/retry/lost` and `/retry/conflict` (1 each, see "Scenarios" below) |
| external tokens | an `externalTokens[]` entry with a `trade` block (a **KRON** token, program `KronToken2433` / `KronToken2732`; the soak cannot issue KRON) is traded on its KAS book by the traders: `trade.bookShare` of their actions, every KAS-book action of their mix (ladders, renewals and merges included; no pair orders), priced around the book's touch midpoint when it lies within 20 % of `trade.refKas` (KAS per whole token), else around `refKas`. No market maker and no fan-out: the traders' bids fill only against sells of holders, so the owner gives the trader keys some of the token (see "Scenarios") |

**Token consolidation** (`consolidate` in the config; the market maker per book, the traders too). Every fill, refund and fan-out leaves a token UTXO with a ~10 KAS carrier, so a bot wallet fragments. After each ladder pass (each trader action) a bot counts its plain, unreserved token UTXOs per token; above the fan-out target (`token2.fanout`: 8 for the market maker, 4 for a trader; TUSD 8 / 4) plus `slack` (8) it merges the **smallest** ones in a chain of `sendTokens` transactions within the program's token inputs (3 for `KCC20Ref`): the first merges 3 UTXOs into one, each next one spends that (not yet accepted) output with 2 more, so a pass of k transactions leaves one UTXO in place of 2k + 1 (`selectMergeChain`); fee and change come from the freed carriers (no KAS UTXO is spent), and the visible count never drops below the fan-out target (so the fan-out is not undone and parallel sell-side placements keep their inputs). At most `maxTxPerInterval` (8) merge transactions per `intervalSec` (60) per key and token; counters `tx_ok:consolidate`, `consolidate_merged:<ticker>`, `consolidate_freed_sompi`, gauge `token_utxos:<key>:<ticker>` in `run/stats/bots.json`. `"consolidate": {"enabled": false}` switches it off, `"traders": false` leaves the traders out.

Sizes are half a dollar to the trader's `maxUsd` (3 to 8 USD; 1 TUSD is about 22 KAS), in every book (an asset token at its USD reference), any amount of base units.

### Scenarios (2026-10-13 gap review)

Flows the soak did not drive before, added to the bots with weights in the config, counters in `run/stats/bots.json` and, where the flow has a
checkable property, a record in `run/state/scenarios.jsonl` that the checker verifies on its own (invariant 13, `src/checker/scenario-rules.ts`).
Everything goes through the web code a user runs (planners, My orders' actions, the x402 SDK).

| scenario | weight (default) | what the bot does | counters (`bots.json`) | checker |
|---|---|---|---|---|
| limit ladder | `ladder` 3 | the ticket's limit ladder (`web/src/ui/ticket/ladder.ts` `expandLimitLadder`): 2 to 20 rungs (70 % 2 to 6, the rest 7 to 20), a percent step of 3 to 40 bps or an end price 0.3 to 3 % away, the first rung 5 to 40 bps behind the reference; one size per ladder (twice the trader's `maxUsd`, split evenly, at least half a dollar), GTC or GTD 30 to 90 min; every rung its own order and transaction, placed in turn (2 s apart); a rung refused or not submitted stops the ladder | `try:ladder` / `placed:ladder` per rung, `ladder_placed`, `ladder_rungs`, `ladder_stopped`, `plan_refused:ladder:<code>` | `ladder`: distinct orders and transactions, no parent, the ladder's token and side, each rung at its price (an auctioning rung: its end price), the sells of one size, prices stepping away from the market |
| ladder: cancel one rung | `ladderCancel` 2 | My orders' cancel of ONE live rung of one of the trader's ladders (with two live rungs at least) | `ladderCancel_ok` / `_failed`, `tx_ok:ladderCancel` | `ladder`: the cancel's transaction has no event of any other rung; a rung still live at the outpoint its cancel spent after 30 min is a `cancel-all` warning (a cancel lost to a fill is fine) |
| GTC renewal | `renew` 2 | My orders' renew (`buildReplacement` with `renew`, then `planCancelReplace`): the OLDEST live GTC order the web lets the user renew (a plain limit: in place; a stop / take-profit / OCO: cancel-replace), same terms, a fresh 90-day expiry. A user renews before day 85; the soak's orders are hours old, so the oldest stands in for the one due first | `renew_ok` / `_failed`, `tx_ok:renew`, `plan_refused:renew:<code>` | `replace`: the planned replacement keeps every term but `expiryDaa`, which grows; on chain the order (an `amend` event in that transaction, or a new order created by it) holds exactly the planned terms (fills, arming and custody aside); a later amend makes it `superseded` |
| Merge tokens | `mergeTokens` 1 | My orders' "Merge tokens" (`web/src/kob/merge.ts` `planMerge`): every plain UTXO of the book's token into ONE by a chain of transfers (3 -> 1, then 1 + 2 -> 1, ... at most 10 links), each later link rebuilt once the previous output is accepted (read from the node, as `nextMergeLink`); a KRON merge adds a KAS input. The fan-out (every 10 min) spreads the trader's tokens again | `mergeTokens_ok`, `_links`, `_inputs`, `mergeTokens_skipped:<code>`, `mergeTokens_link_not_accepted`, `tx_ok:mergeTokens` | `merge`: the record's inputs are distinct and two or more, its total their sum, its final output the last link's; the indexer lists that output for the wallet with exactly that total |
| pair GTD / day / timed | `pairWeights` `gtd` 2, `day` 1, `timed` 2 | a resting pair limit 0.2 to 1 % behind the reference: GTD 30 to 120 min (keeper refund of A or B), a day order, or inactive for 2 to 6 min then GTD 30 to 90 min from its activation; never turned into a taker | `try:` / `placed:pair:gtd@TETH/TUSD` (and `day`, `timed`) | the existing invariants (custody, kill / refund timing, pair fills) |
| pair amend | `pairWeights` `amend` 2 | the pair ticket's replace of a resting pair limit (`intentFromOrder`, `replaceEnv`, `planOrder`, `planCancelReplace` with the maker's B UTXOs): a new price 5 to 25 bps further from the market, same pair, side, size, tip, minimum fill and lifetime | `pair_amend_ok` / `_failed`, `tx_ok:pair:amend@<A>/<B>` | `replace`: the plan keeps pair, side, size, tip and minimum fill; on chain the new KobPair (created by that transaction) holds the planned terms |
| pair cancel-all | `pairWeights` `cancelAll` 1 | My orders' cancel-all of every live pair order of the pair (entries and exits together, up to 6 per transaction) | `pair_cancelAll_orders`, `tx_ok:pairCancelAll@<A>/<B>` | `cancel-all`: every order ends (cancelled, or spent by a fill that won the race); one still live at the very outpoint the cancel spent after 30 min is a warning |
| x402 `/retry/drop` | `x402.weights` 1 | 0.5 KAS native: the merchant closes the connection of the FIRST paid request of each payment before the paywall sees it; the SDK re-sends the same signed artifact, which settles | `x402_paid:/retry/drop`, `x402_resent:/retry/drop`, `x402_fault:/retry/drop:drop` | `x402`: settled exactly once (as every payment) |
| x402 `/retry/lost` | `x402.weights` 1 | 0.25 TUSD: the paywall settles the first paid request, then the merchant answers 500 instead of the resource; the SDK re-sends the same artifact and the paywall answers from memory (`replayed`) | `x402_paid:/retry/lost`, `x402_resent:/retry/lost`, `x402_fault:/retry/lost:lose` | `x402`: settled once; a resent lost answer is served `replayed`, never as a new payment |
| x402 `/retry/conflict` | `x402.weights` 1 | 0.5 KAS paid with TUSD: right after the swap quote names a market-maker bid, the market maker amends that bid (the web's in-place amend, one tick lower), so the facilitator's broadcast meets a spent order (`order_conflict`); the SDK rebuilds with a fresh quote (the raced outpoint left out) under a new payment id, spending the first attempt's anchor | `x402_paid:/retry/conflict`, `x402_rebuilt:/retry/conflict`, `x402_race:amended` / `failed` / `noBid` | `x402`: no superseded attempt is accepted in the facilitator ledger (two attempts of one payment never both settle) |
| KRON book | `externalTokens[].trade.bookShare` | see "external tokens" above | the action counters with `@<ticker>` | every existing invariant over its orders and fills (custody of KRON custodies, all-in, triggers, agreement); no supply check |

The report has a `scenario` line: per kind `ok/checked (recorded)`, and the x402 retry payments with how many were resent, rebuilt or not
exercised (a fault that did not fire: a race that found no market-maker bid, an amend that lost to the market maker's own loop).

**A KRON token on TN10.** KOB cannot issue KRON (its issuance builds KCC-20 only; KRON tokens come from KRON's minter, which has no TN10
deployment). Once one exists on TN10 (minted by KRON's tooling, no live minter), list it and let the traders trade it:

```
"externalTokens": [
  { "ticker": "TKRON", "name": "TN10 KRON test", "covenantId": "<64 hex>", "program": "KronToken2433", "decimals": 8,
    "trade": { "bookShare": 0.05, "refKas": "0.1" } }
]
```

`soak setup` (the registry writer) adds the KRON program to the soak registry (`kron-2433` / `kron-2732`, the shipped registry's pins;
the token with extension commitment null, class `none`), which is the executors' allowlist. Send the trader keys some of the token from the
holder's KRON wallet, then restart the executors (allowlist) and the bots. Without holders the traders' KRON bids rest unfilled and their sells
are refused for lack of tokens (`plan_refused:<action>@TKRON:<code>`).

**What to watch on the next TN10 run** (this build adds the flows above; none has run on TN10 yet):

* `run/reports/latest.txt`, line `scenario`: every kind should reach `ok` with no `replace` / `ladder` / `cancel-all` / `merge` incident;
  `renew` and `pairAmend` mostly `ok` (some `superseded` or `not landed` when a fill or another amend wins), `merge` rarely `output not found`
  (the merged output spent before the check and beyond the owner's spent listing).
* `bots.json`: `ladder_placed` vs `ladder_stopped` (a long ladder of bids can run out of KAS UTXOs: `plan_refused:ladder:<code>`; short
  ladders should place in full), `renew_ok` vs `plan_refused:renew:*` (an in-place ask / bid renewal is the protocol's `AmendOrder`: a
  refusal names the term the builder would not change), `mergeTokens_ok` with `mergeTokens_links` above `mergeTokens_ok` (chains, not only
  3 -> 1), `pair_amend_ok` vs `plan_refused:pair:amend@*`, `pair_cancelAll_orders`.
* x402: `x402_resent:/retry/drop` and `x402_resent:/retry/lost` close to `x402_paid:` of the same paths; `x402_rebuilt:/retry/conflict`
  with `x402_race:amended` (the facilitator's order conflicts grow by about as much); `x402_error:/retry/*` should stay rare; the report's
  `x402 retry` part with few `not exercised`.
* The market maker's ladder after `/retry/conflict` amends one of its bids (the level is re-quoted on its next pass), and the traders'
  `token_utxos:<key>:<ticker>` back at the fan-out target within 10 min of a merge.
* The executors on the pair amend (a KobPair cancel-replace) and on the day / timed pair limits (kills and refunds at their times, invariant 4).
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

`setup` is idempotent (state in `run/state.json`). Stops need no setup: a stop arms or trails in the transaction that fills a
resting ask / bid of the same token (touch trigger, checker invariant 5).


## Operate

| | |
|---|---|
| status | `powershell -File scripts/status.ps1` (children, restarts by cause: `requested` operator restarts / holds, `startFail` exits within 5 s of a start = a deploy step such as a missing bundle, `crashes` = any other exit; `node scripts/restarts.mjs` classifies `supervisor.log` the same way; memory, indexer health, the latest report) |
| stop | `powershell -File scripts/stop.ps1` (creates `run/STOP`; the supervisor stops every child) |
| start / restart | `powershell -File scripts/start.ps1` (state, databases and the record logs persist; executors resume from their cursor) |
| one child only | `powershell -File scripts/start.ps1 -Only miner,exec-a,exec-b` |
| restart one child | `powershell -File scripts/restart-child.ps1 -Name miner` (run/RESTART-<name>: the supervisor restarts only that child, its spec rebuilt from a fresh read of config.json; the others keep running) |
| hold one child down | `powershell -File scripts/hold-child.ps1 -Name exec-b` (run/HOLD-<name>: the supervisor stops it and keeps it down; `-Release` starts it again; refuses to hold the second executor). A child whose binary or bundle is missing waits instead of crash-looping. Use this in a deploy instead of renaming `dist/soak.mjs` or pointing `executorBin` at a binary that exits (a supervisor started before 2026-10-11 ignores the HOLD files) |
| swap the miner binary | rename `run/bin/tn10-miner.exe` (a running exe can be renamed, not overwritten), copy the new one in, `restart-child.ps1 -Name miner` |
| miner watermarks | edit `run/miner.json`: the running miner re-reads it on change (see "Miner" below); `config.json` `miner` is the persistent value |
| pause a matcher | create `run/exec-a/pause` (delete to resume) |
| logs | `run/logs/<child>.log` (+ `.1` .. `.5`), `run/logs/supervisor.log` |
| report | `run/reports/latest.txt` / `.json` (hourly copies alongside), or `node dist/soak.mjs report --config run/config.json` |
| incidents | `run/incidents.jsonl` (one line per invariant violation) |
| bot counters | `run/stats/bots.json`; every bot transaction with fee and masses: `run/state/txs.jsonl`; x402 payments: `run/state/x402-payments.jsonl` |
| resources | `run/stats/resources.json` (latest), `run/stats/resources.jsonl` (one sample a minute) |
| UI | http://127.0.0.1:8490/#/market/<token covenant id> (one market page with BASE / QUOTE selectors: TETH/KAS, TBTC/KAS, KAS/TUSD for the USD reference token, flip toggle), the pair `#/pair/<TETH id>/<TUSD id>` (read-only; connect a TN10 wallet to trade). The UI shows no verification or officialness of tokens (issuance is permissionless); it serves the soak registry from `run/registry/tokens.json` and is built by `scripts/build-ui.sh` |
| cancel an order | `node dist/soak.mjs cancel --config run/config.json <covenant id> ...` (the maker's key, the web app's cancel path) |
| balances | `node dist/soak.mjs balances --config run/config.json` (JSON: per key the node's KAS total, spendable KAS, and per token the proven holdings: UTXO count, amount, KAS locked in carriers; read-only) |
| merge every holding | `node dist/soak.mjs consolidate --config run/config.json [<key> ...]` (redeploy helper, bots stopped: each key's plain token UTXOs merged into ONE per token in rounds of up to 8 inputs, fee and change from the freed carriers; the merged output carries 10 KAS, or 1 KAS when the inputs carried less; the outputs are created after the new indexer's start, so it lists them) |
| carry the history of re-issued tokens | `node dist/soak.mjs carry-history --config run/config.json --db run/<exec>/data/index.sqlite3 --from-registry <old tokens.json> [--to-registry <file>] [--dry-run]` (that executor STOPPED; the new registry defaults to `run/registry/tokens.json`): by ticker, moves the `fill` events (trades, candles, 24 h stats, last price) and the pair fills of each old covenant id to the new id of the same ticker in ONE transaction, so the market pages keep one chart per ticker; orders, token UTXOs and the other events keep the old id. Refuses an old id that still has open / partial orders; records each carry in `meta` (`carried_history:<old id>`), so a re-run is a no-op and a different target is refused. `kob-executor index replay` undoes it (the database is the record log's derived view): run it again after a replay (`src/history-carry.ts`, `test/history-carry.test.ts`) |
| chart history | `node scripts/chart-history.mjs [--only exec-b] [--json]` (read-only; the executors may run): per executor, every registry token's KAS chart and every pair chart must start, at 1d / 1h / 5m / 1m, at the bucket of the first trade in that executor's database, and no fill may be left under an id carried to a current one (a replay of the record log undoes `carry-history`); exit 1 and one `PROBLEM` line each otherwise. A chart that starts later than its database lost history in the read path, not in the data (2026-10-14: the indexer read only the newest 20 000 fills of a candle window) |
| import older runs' history | `node scripts/import-history.mjs --db run/<exec>/data/index.sqlite3 --source <label>=<older index.sqlite3> ... [--old-registry <tokens.json> ...] [--dry-run]` (that executor STOPPED: the script refuses a live writer): each run starts its indexers on a fresh database, so the trades of earlier runs live only in their databases. Copies their KAS-book fills (protocol v2 lot layouts converted exactly) by ticker onto the current covenant ids, each source only until the next run started (the run live at the time wins), each (order, transaction) once, in ONE transaction per source, recorded in `meta` (`history_import:<label>`: a re-run is a no-op); the copied orders are unlisted, nobody's (`maker` NULL) and never reverted (block 0). Writes the first trade per ticker to `run/state/chart-floor.json`, which `chart-history.mjs` holds every database to (`--record-floor` rewrites it from one database). A replay or a rebuilt database drops the import: run it again (`src/history-carry.ts`, `test/history-carry.test.ts`) |
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
  no GPU). Without them it mines continuously. `maxDuty` (0..1] caps the mining time over `dutyWindowSec` (600).
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
  TETH's and TBTC's references are `ETHUSDT` and `BTCUSDT` from the same endpoint over `KASUSDT`: **independent real
  feeds**, not synthetic ratios to TUSD, so the pair prices move the way two real assets do (ETH, BTC and KAS move independently).
  Resting pair orders then drift in and out of the money and the route's implied quotes move, which is what the pair book, the pair
  planner and the matcher's route ranking have to handle; a fixed ratio would freeze the pair price (a resting pair order would either
  always or never fill) and exercise none of it.
* **Checker details.** Invariant 11 reads `detail.pair` of a pair fill (`price_source` none; an ask receives at least
  `ceil(n × q / scale(A))` of B, a bid pays at most `floor(n × q / scale(A))`, q in B base units per whole A); invariant 12 counts the
  indexer's token UTXOs plus the genesis / pre-run holdings it never saw (while they are in the node's UTXO set) and must persist 3
  rounds. The report shows one market line per token, the transactions that filled several books and pair orders (counterparty split
  route / netting / inventory, trigger modes) and the supply deltas.
* **KRON test token: not issued by the soak.** KOB's issuance (`kob-protocol::issue`, `kob token issue`, the web issuance) builds KCC-20 only; a KRON token
  is minted by KRON's own tooling through its minter program, which has no TN10 deployment. A KRON token minted on TN10 is traded by the bots once it is
  listed as an external token with a `trade` block (see "Scenarios"); until then the KRON family stays covered by the executor's KRON twin tests.
* **Stops arm on a touch.** A conditional order or stop entry arms (or trails) only in a transaction that fills a plain resting ask (bid) of the
  same token that quoted at or beyond the stop, trades at least `minTouch` base units and was exposed for `minRestDaa` DAA (default 50). The checker
  (invariant 5) asserts every `arm` / `trail` event is backed by such a fill in the same transaction; a candidate whose exposure the indexer
  data cannot prove is a `warn`, no candidate is an error.

## Redeploy

The soak runs from copies (`run/bin`, `run/wasm-node`, `run/web-dist`, `dist/soak.mjs`), so a new build is staged first and swapped
in one child at a time. When order templates change, the orders on the changed templates are cancelled with the OLD bundle before the
switch (a build neither lists nor builds for a template it does not pin: `docs/spec/template-retirement.md`); orders on unchanged
templates stay. Keys, tokens and databases are kept unless the database schema or the token program changes.

1. Build the new executor, bindings, bundle and UI into a staging directory (`run/stage-<commit>/`). Back up `config.json`,
   `state.json`, the registry, the trackers, the checker state and the databases (`VACUUM INTO` snapshots) into
   `run/backup-pre-<commit>/`. Run `scripts/chart-history.mjs` and keep its output (`--json`) with the backup: it must report no
   problem before the swap, so one after it is the new build's.
2. Hold the bots and the checker (`hold-child.ps1`; with a supervisor started before 2026-10-11, move `dist/soak.mjs` away and run
   `restart-child.ps1`), pause both matchers (`run/exec-*/pause`), and cancel the live orders on changed templates with the old
   bundle (`node dist/soak-<old>.mjs cancel ...`) until both indexers list none.
3. Swap `run/wasm-node` and `dist/soak.mjs`, point `executorBin` at the new binary, run `soak.mjs setup` (it rewrites the registry),
   restart exec-b and exec-a one after the other (never both down), resume the matchers, release the bots and the checker, swap
   `run/web-dist` and restart the UI.
4. After each executor's restart, `scripts/chart-history.mjs --only <exec>` once it has caught up: every chart still starts at its
   first trade (a `kob-executor index replay` or a rebuilt database needs `carry-history` again before the executor serves).
   Watch `status.ps1`, `scripts/activity.mjs` and the report for half an hour: rejected transactions, indexer rejects, new incidents.

**New token program.** A token of a program the build does not pin is not tradable, so a program change issues the soak tokens
again: wind the old run down with its own bundle (cancel every order, sweep strays, `consolidate`), remove `token*` and
`genesisOutputs*` from `run/state.json` (keep a backup), run `setup` (it issues every soak token under the new program and rewrites
the registry), and carry the chart history to the new covenant ids with `carry-history` on each stopped executor's database
(Operate), and import the older runs' history again if the database was rebuilt (`import-history.mjs`). `setup` and the registry writer refuse a `state.json` whose tokens carry another program. An executor started at the sink
(`--start sink`) never saw the holdings created before it: seed `run/state/tracker-<key>.json` with each key's token UTXOs
(`scripts/holdings.cjs --seed`).

## Latest run

**Chart history, 2026-10-14 13:13 to 13:40 UTC.** The founder saw the TUSD chart begin on 10-11. No data was lost: every
fill since 10-06 was in both databases (carried to the re-issued ids on 10-10), but the indexer read only the newest 20 000 fill
rows of a candle window (`MAX_FILL_ROWS`, there since the public snapshot), so once a token passed 20 000 fills its 1d / 1h charts
(and the pair charts derived from them) started at the 20 000th newest fill, about 3.5 days back, sliding forward as fills
accumulated. Fixed in the indexer (candles stream every fill of the window and hold only the newest `limit` candles;
`tests/market_row_bound.rs`) and deployed as `bin/kob-executor-cf90250-chart.exe` (cf90250 plus the fix), exec-b then exec-a.
The history of the earlier runs (m5 from 2026-09-29, m9 in two database generations, same chain and same token ids until the
10-10 re-issue) was imported into both databases with `scripts/import-history.mjs` (TUSD 40 775, TETH 20 772, TBTC 14 415 fills),
each executor parked in turn on a stand-in that blocks (`bin/hold-sleeper.exe`: the running supervisor predates `HOLD` files).
`scripts/chart-history.mjs`: every chart on both indexers starts at its first trade (TUSD 2026-09-29 14:35, TETH 2026-10-01
00:11, TBTC and TBTC/TUSD 2026-10-03 02:09, TETH/TUSD 2026-10-01 00:11 UTC); the public page shows the same at 1d and 1h.

**Redeploy on main cf90250, 2026-10-13 17:16 to 17:20 UTC (2026-10-14 JST).** 121 commits (kob-cli trading commands, the
web private-key signer, matcher weighted selection, indexer open tokens and holdings, x402 `/supported`); no template or
token program changed, so no order was cancelled; databases kept (`order_amends.min_fill` migrated in place), chart history
unchanged. exec-b, then exec-a on `bin/kob-executor-cf90250.exe` (never both down), then bindings, bundle (the new
scenarios run with their default weights), UI (`UI_OUT` staging; `build-ui.sh` now copies `signer.html` and `serve-ui.mjs`
sends the signer's headers) and the public 8491 instance; miner and cloudflared untouched. Backups in
`run/backup-pre-cf90250/`. Live test report: `docs/testing/TN10-TEST-2026-10-14.md`.

**Redeploy on main 8c1606c, 2026-10-13 UTC.** Token-program profiles, the A1-2 / A1-3 fixes and the keeper fee-subsidy
budget; templates unchanged (no order cancelled, chart history and databases kept); one child at a time. Live test report:
`docs/testing/TN10-TEST-2026-10.md`.

**Redeploy on main b937ebf, 2026-10-12 16:21:57 to 16:37:20 UTC (2026-10-13 01:21 to 01:37 JST).** The pre-audit covenant fixes
changed 10 of the 15 order templates (`KobCondAsk`, `KobCondBid`, `KobIfdAsk`, `KobIfdBid`, `KobCondPair`, `KobIfdPair` and the
four KRON conditional / if-done ones); `KobAsk`, `KobBid`, `KobPair`, their KRON twins and the router did not. The 75 live orders
on changed templates were cancelled at 16:22 to 16:24 UTC (anything that probes soak orders should re-read the books after that
time); the databases were kept. Tokens: TUSD, TETH and TBTC on `KCC20Ref` (registry `kcc20-ref-3x3`, issued 2026-10-10), and
**KOBKS** (`4c9948013250170cd5f2c5711392380376615ad357ef67c355dc352da3c164c6`), a token launched with Kasseal's own launch code
on the reference's published public-mint build (`kcc20-ref-public-mint`), listed through `externalTokens` and traded by hand with
`soak.mjs order` / `cancel --keys run/kasseal-keys.json`: an ask, a bid, a matcher fill, a cancel, and a stop-market sell armed by
a fill and then filled against a bid, all listed by both indexers and confirmed by Kasseal's indexer.

What the runs taught (each run's record is in the git history of this file):

* **Zero spread never fills.** A crossing at the same price (bid = ask, no tips) leaves the matcher nothing for the fee, so it stays
  crossed. Give a spread or a tip.
* **IOC and indexer lag.** An IOC lives 300 DAA (30 s) by default; with the indexers 30 to 60 s behind, an IOC can be killed before
  a matcher's book holds it. Rest a crossing limit instead, or give a longer `life`.
* **3 token inputs / 3 outputs.** A sell placement takes at most 3 token UTXOs and a matcher batch at most 3 token legs of one
  token; the bots merge (`consolidate`) and fan out (`fanoutPlan`) in chains for that reason.
* **A token UTXO received outside KOB** (a launchpad curve buy) is unknown to the wallet's tracker: `order --seed
  <txid>:<index>:<amount>:<carrier>` announces it (Kasseal's API lists a key's cells: `GET /v1/tokens/<id>/cells?owner=pk:<x-only key>`).
* **Marketable limits are takers.** A trader limit that crosses the opposite best price is placed as an IOC at its limit
  (`src/bots/taker.ts`, counter `taker:<tag>`); the market maker keeps resting `crossing: 'limit'` ladders.
* **Self-trade refusals.** After a self-trade refusal the bots cancel the refusal's own orders and plan again without them; a retry
  planned from a lagging listing would meet the cancelled order again.
