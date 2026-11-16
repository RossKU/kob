# kob-mm-bot

A market maker for a dollar-stablecoin token on its KOB KAS book: set `token` to its covenant id. It quotes a bid
and an ask ladder around a fair value taken from the KAS/USD exchange markets, amends its orders in place when the
market moves, and never takes an order. It drives the `kob` CLI as a child process and needs Node 22; there are no npm
dependencies. The token is pinned by covenant id, never by ticker (tickers collide).

## Setup

1. Get the `kob` CLI (a release build checked against `SHA256SUMS`, or `cargo build --release --locked --bin kob`).
2. Make a key used **only by the bot**, in a file readable by the bot's user only (or in `KOB_KEY`). The bot treats every
   order of the token on that key as its own: an order it does not know is taken for a stale quote, cancelled and logged.
3. Fund the key's address with KAS (bids and a 50 KAS reserve) and the token (asks). With the defaults each side needs
   about 450 USD. Fund the token first if no KOB order of the token exists yet: until then only a holder can buy it.
4. Copy `config.example.json` to `config.json` (git ignores it), edit it, and check it with `node bin/mm-bot.mjs check --config config.json`.
5. Try one cycle that signs nothing: `node bin/mm-bot.mjs once --config config.json --dry-run`.

## Running and stopping

```sh
node bin/mm-bot.mjs run    --config config.json          # the bot, restarted 60 s after a crash
node bin/mm-bot.mjs status --config config.json [--json] # mode, fair value, venues, quotes, inventory, fees today
node bin/mm-bot.mjs stop   --config config.json          # writes <dataDir>/STOP
node bin/mm-bot.mjs once   --config config.json          # one cycle (add --dry-run to sign nothing)
```

- **Stop:** the STOP file, `stop`, or Ctrl-C/SIGTERM. The bot cancels every quote, waits until none is listed, and exits.
  Delete the STOP file to start again.
- **It never stops by itself.** Every safety condition makes it IDLE: quotes pulled, nothing new sent, and it resumes on
  its own. Errors are retried with a backoff (15 s doubling to 5 min).
- One bot per `dataDir`.

## How it works

Every 30 s it reads the reference, the wallet (`kob balances`) and its live orders (the indexer), syncs its state file,
picks the mode, and quotes or pulls its quotes.

- **Reference.** For Bybit, KuCoin and Gate (KAS/USDT, times Kraken USDT/USD) and Kraken KAS/USD: a VWAP of the last 5
  closed 1-minute candles. Median of the venues; a venue more than 1 % off the median, stale or silent is dropped; MEXC is
  a fallback. Fewer than 2 venues: IDLE.
- **Fair value** = token USD / KAS USD; token USD is Kraken's USDC/USD (fallback 1.0). KAS/USDT is used because it
  trades about USD 1 million a day per venue against USD 5k to 75k for KAS/USDC; the USDC books are only cross-checked
  in the log.
- **Ladder.** `spreadPct` (1 %) is the gap between the best bid and the best ask, so each sits 0.5 % from fair; further
  levels step out by `levelStepPct` (0.5 %). Sizes are % of `capitalUsd`: 15 % a level, at most 50 % a side.
- **Repricing.** An order is amended in place (`kob replace <id> --price`) only when fair has moved more than 0.25 % since
  it was quoted, never on a timer. Cancel and place only when an amend cannot do it: an order shrunk by a fill, an ask
  whose carrier is too low, or a level no longer quoted. Free token UTXOs are merged before an ask.
- **Inventory.** A side without inventory stops quoting; the bot never borrows, shorts, or crosses itself.

| Mode | When |
|---|---|
| QUOTING | reference fresh and steady |
| IDLE `reference` | fewer than 2 fresh venues |
| IDLE `jump` | the reference moved more than 2 % within a minute |
| IDLE `peg` | USDC/USD more than 0.5 % from 1.0 |
| IDLE `fee-cap` | the day's fees reached the cap less the cancel reserve; resumes at 00:00 UTC |
| IDLE `cooldown` | the condition cleared; quoting resumes after 3 min of a steady reference |
| STOPPING | the operator's STOP or a signal |

**Deposits.** When a side is short the bot says what to send, once per change and every 60 min while it lasts:

```
IDLE: Insufficient USDX (<covenant id>): asks need 45% = 450 USD = 450 USDX, 0 USDX (0%) held; deposit 450 USDX to kaspa:qq... to resume asks.
INFO: Funds received, ask quoting resumed.
```

**New bridge instance.** Set `token` to the new covenant id and move the old one to `retiredTokens`. For a retired token
the bot only cancels its orders and merges the returned UTXOs, then logs "retired instance ...: quotes pulled, N tokens
back in the wallet". Moving them through the bridge is manual.

## Watching the bot with Claude

Every alert is one JSON line in `<dataDir>/logs/alerts.jsonl`: `ts`, `level` (`info`, `warn`, `idle`, `error`), `state`,
`reason`, `message` (one sentence with the action needed), `asset`, `shortfall`, `address`.

- In a Claude Code session, run `tail -f <dataDir>/logs/alerts.jsonl` under the Monitor tool and ask it to report every
  `warn`, `idle` and `error` line.
- Or poll `node bin/mm-bot.mjs status --config config.json --json`.

An optional `alerts.webhookUrl` receives the same JSON as a POST (off by default).

## Configuration

One JSON file; omitted fields take the default; relative paths are relative to the file.

| Field | Default | Meaning |
|---|---|---|
| `network` | `mainnet` | `mainnet` or `testnet-10` |
| `node` | `null` | wRPC URL; `null`: `KOB_NODE_WRPC`, else the CLI's two public nodes |
| `indexer` | `null` | KOB indexer URL; `null`: the network's public one |
| `kobCli` | `kob` | path of the CLI |
| `keyFile` | `null` | key file; `null`: `KOB_KEY` |
| `dataDir` | `./run` | state, logs, STOP |
| `capitalUsd` | `1000` | capital base (100 %) |
| `token.covenantId` | required | the token's covenant id |
| `token.decimals` | required | checked against the CLI |
| `token.label` | `null` | name in messages |
| `token.usdSource` | `kraken:USDCUSD` | token USD rate: `kraken:<PAIR>` or `fixed` |
| `token.usdFallback` | `1.0` | rate when the live one is stale; `null`: IDLE |
| `token.pegUsd`, `token.pegBand` | `1.0`, `0.005` | IDLE `peg` outside the band |
| `retiredTokens` | `[]` | old instances: recovery only |
| `reference.pollSec` | `30` | seconds per cycle |
| `reference.windowMin` | `5` | closed candles per VWAP |
| `reference.venues` | bybit, kucoin, gate, kraken | primary venues |
| `reference.fallbackVenues` | mexc | used below 3 fresh primaries |
| `reference.minVenues` | `2` | fewer: IDLE |
| `reference.maxDeviation` | `0.01` | outlier distance from the median |
| `reference.maxCandleAgeSec` | `120` | older newest candle: stale |
| `reference.timeoutMs` | `10000` | per request |
| `reference.usdtUsd` | kraken, coinbase | USDT/USD sources |
| `reference.stableMaxAgeSec` | `300` | max age of a stablecoin rate |
| `reference.crossCheck` | on, 300 s | KAS/USDC log-only check |
| `quotes.levels` | `3` | levels per side |
| `quotes.spreadPct` | `1.0` | best bid to best ask, % of fair |
| `quotes.levelStepPct` | `0.5` | step per further level, % of fair |
| `quotes.levelSizePct` | `15` | level size, % of capital |
| `quotes.maxAskPct`, `quotes.maxBidPct` | `50`, `50` | side cap, % of capital |
| `quotes.kasReserve` | `50` | KAS never bid (fees, carriers) |
| `quotes.minOrderKas` | `15` | smallest order |
| `quotes.repriceThreshold` | `0.0025` | fair move that triggers an amend |
| `quotes.resizeBelow` | `0.5` | re-place an order below this share of its level |
| `quotes.askCarrierMinKas` | `0.2` | re-place an ask whose carrier is lower |
| `quotes.mergeAboveUtxos` | `1` | merge free token UTXOs above this count |
| `quotes.skew` | off | `{ enabled, maxShift: 0.002, targetTokenShare: 0.5 }` |
| `safety.killJump` | `0.02` | one-minute move that pulls the quotes |
| `safety.resumeAfterSec` | `180` | cool-down |
| `safety.maxFeesKasPerDay` | `50` | fee cap per UTC day |
| `safety.cancelReserveKas` | `5` | part of the cap kept for cancels |
| `safety.placeGapSec` | `8` | gap between placements |
| `safety.orderVisibleTimeoutSec` | `180` | re-read an action not seen by then |
| `safety.maxActionsPerCycle` | `12` | per cycle |
| `safety.backoffBaseSec`, `backoffMaxSec` | `15`, `300` | retry backoff |
| `alerts.warnBelowPct` | `12.5` | WARN below this side holding, % of capital |
| `alerts.remindMin` | `60` | reminder interval |
| `alerts.webhookUrl` | `null` | optional POST target |
| `cli.timeoutSec` | `180` | per CLI call |
| `cli.preflight` | `true` | `--dry-run` before every `--yes` |
| `cli.priorityFee` | `false` | pass `--priority-fee` |

## Logs

Under `dataDir`: `logs/mm-bot.log` (readable), `logs/actions.jsonl` (every action with tx id and fee, mode changes,
errors), `logs/fills.jsonl`, `logs/alerts.jsonl`, and `state.json` (the bot's orders, fee ledger, mode, last snapshot).

## Fees

Network fees measured on testnet-10 with this bot (2026-11-16, a 6-decimal token on the KCC-20 public-mint program):

| Transaction | Fee (KAS) |
|---|---|
| place a bid / an ask | 0.002234 / 0.010112 |
| amend in place, bid / ask | 0.004168 / 0.004276 |

An amend's fee comes from the order itself (an ask's 1 KAS carrier, a bid's escrow). The default cap of 50 KAS a day is a
guard against a runaway, not an expected cost.

## Risks

- A maker is filled when the price moves against it; the 5-minute VWAP lags fast moves, and quotes rest between cycles.
- The bridge is not monitored: a bridge pause, a closed deposit route or a Circle blacklist would not move USDC/USD, so
  the bot would keep quoting. Watch the bridge and stop the bot if the backing is in doubt.
- Exchanges that agree on a wrong price are followed; one bad venue is not.
- KOB, the CLI and this bot are pre-release and unaudited, and the CLI's JSON may change: pin the CLI version and dry-run
  after every upgrade.
- The key is a hot key that signs without asking; keep on it only what the bot needs.
- A cancel can lose a race with a fill.

## Tests

`npm test` (no network, no keys): the pure modules with recorded venue candles, the mode transitions, the deposit math,
the CLI wrapper, and whole cycles against a stand-in CLI.
