# kob-mm-bot

A market maker for a dollar-stablecoin token on its KOB KAS book: set `token` to its covenant id. It quotes a bid
and an ask ladder around a fair value taken from the KAS/USD exchange markets, amends its orders in place when the
market moves, and never takes an order. Every quote has a short life (90 min by default) that the bot renews in place.
The expiry is soft: once a dead bot's quotes pass it, conforming matchers stop filling them, but they stay fillable at
their price until a refund keeper ends them (see Risks). It drives the `kob` CLI as a child process and needs Node 22; there are no npm
dependencies. The token is pinned by covenant id, never by ticker (tickers collide).

## Setup

1. Get the `kob` CLI (a release build checked against `SHA256SUMS`, or `cargo build --release --locked --bin kob`).
2. Make a key used **only by the bot**, in a file readable by the bot's user only (or in `KOB_KEY`). The bot's own orders
   are the ones it placed (its records in `state.json`). Anyone can make an order that names the key, at any price and
   amount and without custody, so an order of the token on the key that the bot did not place is not taken for its own:
   it counts toward no cap and no self-trade check, its cancel is tried at most 3 times (it may be a quote of a lost
   state file), and one alert names it. Cancel such an order by hand if it is yours.
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
node bin/mm-bot.mjs unresolved --config config.json      # the asks kept with no proof that they ended
node bin/mm-bot.mjs drop   --config config.json --id ID [--amount-left N] [--force]  # drop one of them, after checking it
```

On mainnet `check`, `run` and `once` refuse to start without `node` set to a JSON wRPC URL (`ws://host:18110`): the
proof that a fully filled ask paid the bot reads the node, and without it every full fill would park its ask (below).
On testnet-10 it is a warning. They also refuse until `quotes.askPartialFills` is set, `true` or `false` (below, "Partial or whole fills"); on testnet-10 it defaults to `true`.

- **Stop:** the STOP file, `stop`, or Ctrl-C/SIGTERM. The bot cancels every quote, waits until the node shows none left, and exits.
  Delete the STOP file to start again.
- **The bot's own record of each order.** Every placement and amend is recorded in `state.json` with the order's
  state and outpoint as the CLI's own result for that transaction gives them, before the next action. The indexer only
  says where to look: an order it does not list, lists at another outpoint, or that has an open question is looked up
  on the node from that record (`kob recover --from <dataDir>/own-orders.json`). A partial fill moves an ask to a new
  script, and `kob recover` finds it only at an amount it is told (the bot passes every amount left it knows: its own
  record, the indexer's list and per-order view, and the amount left after each fill the indexer reported, at most 32;
  and the amounts the payments to the bot name, below) or reaches by its bounded search (`amountLeft - k x minFill`); a
  fill of another amount is not followed. Cancels go through the
  node from the record too (`kob recover --cancel`); `kob cancel <id>` is the fallback for an order the record cannot
  find. An order leaves the state file only on proof that it ended, under an order template the node has proven (an
  order found live under it): a bid has no unspent output at its script (a bid's script never changes), the node shows
  the bot's own cancel of it confirmed (an output of the cancel's transaction to the bot's address: the order's carrier
  comes back as change; for a cancel-replace, its replacement live on the node), or a placement of unknown outcome is
  shown by neither the node nor the indexer (and the node shows no output of its transaction). A cancel that was only
  submitted is no proof: a fill that lands first leaves the ask live at an amount the search may not reach, so the ask
  stays unresolved, is looked up each cycle and cancelled again once found. Without a JSON wRPC node (testnet-10 only)
  the bot cannot see its outputs, and an ask ends once its cancel went out, as before (the start log warns). An ask the node
  does not find is not proof (it may sit at an amount the search did not reach): it is kept as unresolved, with one
  error alert. Until an order leaves, it counts toward the side caps, its level is not quoted again, it is never
  amended, every pull and STOP try to cancel it, and each cycle it is looked up again.
- **The bot's own transactions in flight.** A placement or an amend the bot sent is in flight until the node shows
  it: an output of that transaction, the order live at its outpoint (for a placement, live at all), or the order live
  at an outpoint that shows it can never confirm (one that is neither the amend's nor the one it spent). Until then
  "not found" proves nothing, so no order ends, and every state the order can be in (its own, the one an amend spent,
  and each one an amend planned, also after an amend was taken for not landed) is looked up each cycle. One in flight
  longer than `safety.orderVisibleTimeoutSec` raises one `own-tx-unconfirmed` alert; the timeout only schedules
  checks, resends and alerts, it never ends an order. An order still in flight past its own expiry is parked (STOP no
  longer waits for it), and 2 h past it the normal rules decide, as for a proven ask. The CLI's note that blocks are
  contended goes to the log: under contention relay-fee KOB transactions may wait, and `cli.priorityFee: true`
  pays for their storage mass (at most about 0.31 KAS more a transaction).
- **A fully filled ask** leaves no output on the node either. It is *proven* when the fills the indexer reports for it add up
  to what its last own outpoint held (the amount at its last placement or amend) and each is paid: for each fill, one
  output to the bot's own address in that fill's transaction, worth at least what the covenant pays the maker,
  `ceil(amount x (price - tip) / scale)`, in a transaction the bot did not send. The output must be one the node first
  showed after that placement or amend (the bot reads its outputs just before each placement and amend), and one that
  proved no other fill: each output proves one fill once, and `state.json` keeps which ask it proved. An output of
  one of the bot's own transactions (its change) is recorded as own and never counts, however long it stays unspent.
  A proof is not bound to the ask: the bot cannot check that the paying transaction spent it (a node returns a
  transaction by id only from its mempool, there is no transaction index over wRPC, and a UTXO read shows no inputs).
  A lying indexer that also trades can therefore name the payout of a fill of another of the bot's asks, or any other
  receipt after the placement (an operator top-up), as this ask's payment. So a proof never makes the bot forget an
  ask. A proven ask gives its ladder level back and no longer counts toward the ask cap (the wallet's real token
  balance, read from the node, bounds what the bot can place), but its record stays in `state.json` with every amount
  hint it has: each cycle a few proven asks (5, in rotation, the ones the indexer lists first) are looked up on the
  node, in every mode, and one found live is cancelled through the node, with an error alert (`proven-live`), and
  counts again until the node shows it ended. When a STOP or an IDLE pull begins (once per STOP, once each time
  quoting stops), every proven ask gets one more lookup, 20 a cycle (one `kob recover --cancel` call of at most 4,000
  candidate scripts), after the pull's own cancels of that cycle, with every hint and the indexer's per-order view
  when it answers (6 such reads at a time); a STOP ends only after that
  pass (at most 10 cycles for 200 records; a record the node does not answer for is tried in 3 calls, then reported),
  and the stop line says how many were looked up, found live and cancelled, and not found, the last listed in
  `<dataDir>/proven-not-found.json` (the shape `kob recover --from` reads). A proven ask is cancelled through the node
  only, never through the indexer's view (`kob cancel <id>`). Until its expiry its price still counts in the self-trade check. A proven
  ask ends when the node shows it ended after that cancel, 2 h after its expiry (past its expiry no conforming
  matcher fills an ask, and keepers refund it), or by `mm-bot drop`. At most 200 are kept. A record lives at most
  `lifeMin` + 2 h (a proof comes after the placement or amend, the expiry at most `lifeMin` after it): 3.5 h at the
  default 90 min, so the cap of 200 is reached only above about 57 full fills an hour, sustained. At the cap a new
  proof frees nothing (the ask stays unresolved, as without proof: its level is blocked until its expiry, then parked
  and still counted in the ask cap) and the bot warns once per ask (`proven-full`). The ladder then runs short on the
  ask side until older records end, within 3.5 h; the operator should check `mm-bot unresolved` and, if fills that
  fast are expected, lower the fill rate (fewer or larger levels) or drop records already seen ended (`mm-bot drop
  --id <id> --force`, bot stopped). A forged proof thus never makes the bot forget an order; what it can still cost is
  below (**Partial or whole fills**). To see the payments the
  bot reads its own address on the node (`getUtxosByAddresses`, JSON wRPC) at the start of each cycle, before each
  placement or amend, and after a cycle that sent. An output keeps the time the node first showed it for as long as the
  node shows it; once the node stops showing it, its record is kept for a week (at most 2,000 that proved nothing and
  are not own; the others all week), so a payment the bot spends later still counts and one read that misses an
  output does not make it new. That is one node call per 30 s cycle, plus one per placement or amend (at most `maxActionsPerCycle`, 12) and
  one after a cycle that sent. It needs `node` set to a JSON wRPC URL (`ws://host:18110`,
  testnet-10 `18210`): with the CLI's default or a Borsh node no full fill is ever proven (`check` and the log warn).
- **When a fill cannot be proven** (no JSON wRPC node; the node read failed; the indexer never reports the fill; the
  payment landed while the bot was sending and the CLI spent it in that same cycle, before the read after the send;
  more than 1,000 fills of the token between two reads, 10 pages of 100, with a `fills-missed` alert; the indexer
  reports it more than a week or 2,000 spent payments late; 200 proven asks kept already) the ask stays unresolved: its level is not quoted
  again until the ask's own expiry, at most `lifeMin` (90 min) after its last placement or amend, so 45 to 90 min with
  the renewals. Past its expiry it is parked: STOP no longer waits for it, the bot keeps looking for it, but its tokens
  still count toward the ask cap until proof, because a non-conforming taker can still fill an expired ask until a
  refund ends it. With the defaults (15 % of `capitalUsd` a level, `maxAskPct` 50 %) one parked ask leaves the ask
  ladder a level short until the operator checks it (`kob recover --amount-left <id>=<amount>`) and drops it with the
  bot stopped: `mm-bot unresolved` lists these asks (and the proven ones), and `mm-bot drop --id <id> [--amount-left <n>]` asks the node
  first, from the bot's record with every amount it knows (and `n`), prints the answer, and refuses while the node
  shows the order live or cannot be asked. The node finds a partly filled ask only at an amount it is told, so check
  the amount left first (`mm-bot unresolved` writes the listed records to `<dataDir>/unresolved-orders.json` for
  `kob recover --from`). Without `--force` it drops an ask only when the node showed that it ended (a cancel the bot
  sent shows on the node) or the record is past the end rule of a proven ask (2 h past its expiry); otherwise it
  refuses and says why: the lookup proves nothing (the order's template is not yet proven on the node), the indexer
  reported fills of the order since its last placement or amend, or it reported none (the most suspicious case: an ask
  leaves its script only by a fill, a cancel or a refund, so the indexer may have withheld a fill); in each case it may
  be live at an amount the search did not reach. A dropped order that is still live stays fillable, and the bot no
  longer cancels it (no pull, IDLE or STOP reaches it): every drop is appended to `<dataDir>/dropped-orders.json`
  (the order's record in the shape `kob recover --from` reads, with its amounts, fills, last transaction, the time and
  why), so `kob recover --from <dataDir>/dropped-orders.json --amount-left <id>=<n> [--cancel]` can still find and cancel it. The limits stay as they are, and bounded: a proof needs only the payments after the ask's
  last placement or amend, at most `lifeMin` (up to 1 day) before its expiry, so the week that spent payments are kept
  covers it unless the indexer reports the fill days late. With a JSON wRPC node and an honest indexer this should be rare: the bot reads its
  outputs every cycle before it sends, so it misses a payment only when it lands within the seconds of one cycle's
  sends and the CLI picks that very output to fund one of them.
- **Amounts from the payments.** With a JSON wRPC node every partial fill of an ask also names its own amount: the
  covenant pays the maker at least `allIn = ceil(n x (price - tip) / scale)` at the ask's output, a partial fill
  leaves both carriers on the order, and a matcher pays exactly `allIn`. So a payment of P sompi names the filled
  amount n (one value while a base unit is worth a sompi or more), whatever the indexer says. Each ask the bot looks
  up is also tried at its last known amount (its own outpoint's, or the node's last live answer) less what each
  payment first seen after it names, and less each sum of 2 or 3 of them: at most 12 payments (the ones the indexer
  names as its fills first) and 160 amounts per ask and lookup, within the 200 candidate scripts `kob recover`
  already searched per order, so the node calls do not grow. A payment that could pay for more than 200 fill amounts
  (a base unit worth under 1/200 sompi) is skipped, with a log line. An ask found that way marks those payments as its
  fills (the log names them); they then prove nothing else.
- **Partial or whole fills (`quotes.askPartialFills`).** `false` places every ask with its minimum fill at its whole
  amount, so a taker takes a whole level and the node alone tells the bot which asks are live. `true` lets takers fill
  an ask in part, and when the indexer the bot reads cannot be trusted, an ask filled to an amount the bot does not know
  can stay at its old price until its expiry (at most `lifeMin`). The remedy is your own node and indexer, or whole fills.
- **An indexer that hides a quote or shows it at an old outpoint.** The node still holds it: it is cancelled through the
  node, with an error alert, and its level is quoted again once the node shows it ended. An order the node cannot find
  while the indexer still lists it, or that cannot be looked up yet (no order template proven: the indexer never listed
  one of the bot's orders), is kept, with an error alert, until the node shows it ended. STOP ends when no order is left
  by that rule, whatever the indexer lists.
  The bot reads the maker list for its token only, newest first, at most 1,000 orders (10 pages of 100). When that cap
  is reached with more left (anyone can post orders under the bot's key), an order the list leaves out may only be
  older: it is not taken as hidden and not cancelled for it, and the bot goes on from its own record and the node.
- **Indexer down.** When a pull is due and the wallet or indexer read fails, the bot cancels through the node from its
  records, with no indexer. While the reads fail, the quotes are also pulled this way once fair has moved more than half
  the spread from the fair they were set at, and the 1-minute jump compares with the newest reading at least a minute
  old, however old.
- **A placement of unknown outcome** (a lost reply) is tracked under the covenant id its plan names until the node says;
  with no id known it counts toward the side caps for a day.
- **It never stops by itself.** Every safety condition makes it IDLE: quotes pulled, nothing new sent, and it resumes on
  its own. Errors are retried with a backoff (15 s doubling to 5 min).
- One bot per `dataDir`.

## How it works

Every 30 s it reads the reference, the wallet (`kob balances`) and its live orders (the indexer), syncs its state file,
picks the mode, and quotes or pulls its quotes.

- **Reference.** For Bybit, KuCoin and Gate (KAS/USDT, times Kraken USDT/USD) and Kraken KAS/USD: a VWAP of the last 5
  closed 1-minute candles. Median of the venues; a venue more than 1 % off the median, stale or silent is dropped; MEXC is
  a fallback. Fewer than 2 venues: IDLE; exactly 2 must agree within `pairAgreement` (0.125 %). USDT/USD is asked of
  Kraken and Coinbase: a reading more than 1 % from 1, or two more than 0.2 % apart, gives no rate.
- **Fair value** = token USD / KAS USD; token USD is Kraken's USDC/USD; while it is missing or stale (`stableMaxAgeSec`) the bot is IDLE and resumes by itself, so the peg band is never skipped. KAS/USDT is used because it
  trades about USD 1 million a day per venue against USD 5k to 75k for KAS/USDC; the USDC books are only cross-checked
  in the log.
- **Ladder.** `spreadPct` (1 %) is the gap between the best bid and the best ask, so each sits 0.5 % from fair; further
  levels step out by `levelStepPct` (0.5 %). Sizes are % of `capitalUsd`: 15 % a level, at most 50 % a side.
- **Repricing.** An order is amended in place (`kob replace <id> --price`) only when fair has moved more than 0.25 % since
  it was quoted. Cancel and place only when an amend cannot do it: an order shrunk by a fill, an ask whose carrier is too
  low, or a level no longer quoted. Free token UTXOs are merged before an ask.
- **Expiry and renewal.** Every placement and every amend carries `--gtd`, an expiry `quotes.lifeMin` (90 min) ahead.
  A quote that has not been amended for `quotes.renewAfterMin` (45 min) is amended in place to its target with a fresh
  expiry (`kob replace <id> --price P --gtd WHEN`): the only timer-driven action. A reprice renews too, so in a moving
  market the renewals cost nothing extra. A quote whose recorded expiry has passed, after a long outage, is renewed at once
  (the expiry and the amount left are the bot's own record, not the indexer's); one the bot has no expiry for (placed by a version without it) is renewed in the first cycle. If the bot
  dies, every quote lapses at most `lifeMin` after the bot's last action: conforming matchers stop filling it and a
  refund keeper returns its KAS or tokens to the key. Renewals obey the fee cap like every amend; at the cap the bot
  idles and pulls its quotes, it does not let them lapse.
- **Inventory.** A side without inventory stops quoting; the bot never borrows or shorts. New placements fit inside the
  side caps less every order it tracks (`maxBidPct`, `maxAskPct` of `capitalUsd`).
- **No self-trade.** A placement or amend that would meet the bot's own order on the other side is skipped, and the
  amends that move a quote away from the other side go first.
- **Decimals.** Every placement names `--decimals` from `token.decimals`; the CLI checks it against every other source.

| Mode | When |
|---|---|
| QUOTING | reference fresh and steady |
| IDLE `reference` | fewer than 2 fresh venues |
| IDLE `jump` | the reference moved more than 2 % within a minute |
| IDLE `peg` | USDC/USD more than 0.5 % from 1.0 |
| IDLE `placement` | the CLI refused 3 placements in a row (an error alert); a dry run of a placement is tried every cycle and the bot resumes once one passes |
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
back in the wallet". Moving them through the bridge is manual. At most 2 such cancels go out a cycle, and none starts
once 20 s of the cycle went to them, so the next reprice of the live quotes waits at most 20 s plus one `kob` call
(`cli.timeoutSec` per step) for them. An order whose cancel fails 3 times (one anyone made under the key, with no
custody) is given up.

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
| `node` | `null` | wRPC URL; `null`: `KOB_NODE_WRPC`, else the CLI's two public nodes. Mainnet needs a JSON wRPC URL (`check`, `run` and `once` refuse without one). The bot passes it to every CLI call too, so it then trusts that one node instead of the CLI's two public nodes that must agree: run your own |
| `indexer` | `null` | KOB indexer URL; `null`: the network's public one |
| `kobCli` | `kob` | path of the CLI |
| `keyFile` | `null` | key file; `null`: `KOB_KEY` |
| `dataDir` | `./run` | state, logs, STOP |
| `capitalUsd` | `1000` | capital base (100 %) |
| `token.covenantId` | required | the token's covenant id |
| `token.decimals` | required | checked against the CLI |
| `token.label` | `null` | name in messages |
| `token.usdSource` | `kraken:USDCUSD` | token USD rate: `kraken:<PAIR>` or `fixed` |
| `token.usdFallback` | `null` | rate to use when the live one is stale (the peg band is then not checked); `null`: IDLE until it is live again |
| `token.pegUsd`, `token.pegBand` | `1.0`, `0.005` | IDLE `peg` outside the band; the band must be at most half the spread (`quotes.spreadPct / 200`), since a wrong token USD rate inside it moves the fair value by up to the band |
| `retiredTokens` | `[]` | old instances: recovery only |
| `reference.pollSec` | `30` | seconds per cycle |
| `reference.windowMin` | `5` | closed candles per VWAP |
| `reference.venues` | bybit, kucoin, gate, kraken | primary venues |
| `reference.fallbackVenues` | mexc | used below 3 fresh primaries |
| `reference.minVenues` | `2` | fewer: IDLE |
| `reference.maxDeviation` | `0.01` | outlier distance from the median |
| `reference.pairAgreement` | `0.00125` | 2 venues further apart: IDLE (below half the spread) |
| `reference.maxCandleAgeSec` | `120` | older newest candle: stale |
| `reference.timeoutMs` | `10000` | per request |
| `reference.usdtUsd` | kraken, coinbase | USDT/USD sources, all asked |
| `reference.usdtAgreement`, `usdtBand` | `0.002`, `0.01` | two sources further apart than `usdtAgreement`, a lone source further than `usdtAgreement` from 1, or any reading further than `usdtBand` from 1: no rate. `usdtAgreement` must stay below half the spread |
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
| `quotes.askPartialFills` | `null` | `true`: asks take partial fills; `false`: each ask is filled in full or not at all, and the node alone tracks it (tracking, "Partial or whole fills"). Mainnet refuses `null`; testnet-10 takes it as `true` |
| `quotes.mergeAboveUtxos` | `1` | merge free token UTXOs above this count |
| `quotes.lifeMin` | `90` | each quote's expiry, minutes after its last placement or amend (30 to 1440) |
| `quotes.renewAfterMin` | `45` | renew a quote not amended for this long; at least 15 min below `lifeMin` |
| `quotes.skew` | off | `{ enabled, maxShift: 0.002, targetTokenShare: 0.5 }` |
| `safety.killJump` | `0.02` | one-minute move that pulls the quotes |
| `safety.resumeAfterSec` | `180` | cool-down |
| `safety.maxFeesKasPerDay` | `50` | fee cap per UTC day |
| `safety.cancelReserveKas` | `5` | part of the cap kept for cancels |
| `safety.placeGapSec` | `8` | gap between placements |
| `safety.orderVisibleTimeoutSec` | `180` | re-read an action not seen by then, resend an unseen cancel, alert on a transaction in flight (30 to 3600; never ends an order) |
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
`mm-bot.log` and `actions.jsonl` move to `<file>.1` past 50 MB. `state.prev.json` is the state before the last save: the
bot refuses to start on a `state.json` that does not read (it would forget its quotes) and names that copy.

## Fees

Network fees measured on testnet-10 with this bot (2026-11-16, a 6-decimal token on the KCC-20 public-mint program):

| Transaction | Fee (KAS) |
|---|---|
| place a bid / an ask | 0.002234 to 0.003354 / 0.010112 |
| amend in place, bid / ask | 0.004168 / 0.004276 |
| cancel a bid / an ask | 0.003806 / 0.012504 |
| merge 5 token UTXOs (2 transactions) | 0.051292 |

The 31-minute run (12 placements, 9 amends, 12 cancels, 1 merge) paid 0.264424 KAS in 34 transactions.

An amend's fee comes from the order itself (an ask's 1 KAS carrier, a bid's escrow). The default cap of 50 KAS a day is a
guard against a runaway, not an expected cost.

**Renewals.** A renewal is an amend in place, booked as `renew` in the fee ledger (`status` shows it under "fees
today"). With the defaults a quote that is never repriced is renewed 32 times a day (every 45 min). At the soak's amend
fees (3.3 days on testnet-10, 2026-11-16 to 11-20: a bid amend 0.006316 KAS, an ask amend 0.004276 KAS) the bound for
6 quotes in a flat market is 32 x (3 x 0.006316 + 3 x 0.004276) = 1.02 KAS a day, 2 % of the cap. Each ask's renewals
also spend 0.137 KAS a day of its 1 KAS carrier, so a never-repriced ask is cancelled and placed again about every 6
days (about 0.07 KAS with a merge). Replayed over the soak's real market, where each quote was repriced about every
15 min, the renewals would have added 60 amends in 3.3 days: 0.10 KAS a day.

## Risks

- A maker is filled when the price moves against it; the 5-minute VWAP lags fast moves, and quotes rest between cycles.
- The bridge is not monitored: a bridge pause, a closed deposit route or a Circle blacklist would not move USDC/USD, so
  the bot would keep quoting. Watch the bridge and stop the bot if the backing is in doubt.
- Exchanges that agree on a wrong price are followed; one bad venue is not (with 2 venues, only within `pairAgreement`).
- KOB, the CLI and this bot are pre-release and unaudited, and the CLI's JSON may change: pin the CLI version and dry-run
  after every upgrade.
- The key is a hot key that signs without asking; keep on it only what the bot needs.
- A cancel can lose a race with a fill. The bot then keeps the order (it ends only once the node shows the cancel's own
  transaction), looks it up and cancels it again once found.
- The trust in the services the bot reads: the indexer (`indexer`) says which orders and fills exist, and
  `node` is the one node the bot and every CLI call it makes then trust (without `node` the CLI asks two public nodes
  that must agree). A lying node alone cannot end an ask (the fills come from the indexer), but a lying node and a
  lying indexer together can invent both the payment and the fill. Run your own node, and an indexer you run or trust.
- The expiry is soft. Past it, conforming matchers and the KOB book no longer use a quote, but the chain cannot bound
  when a fill happens: a non-conforming matcher could still fill it until a refund keeper (or the bot's next renewal or
  cancel) spends it. The short life narrows the window of a dead bot's quotes from 90 days to `lifeMin`; STOP still
  cancels them at once.

## Tests

`npm test` (no network, no keys): the pure modules with recorded venue candles, the mode transitions, the deposit math,
the CLI wrapper, the renewal timing, and whole cycles against a stand-in CLI (among them a dead bot's quotes lapsing and
a restart renewing them).
