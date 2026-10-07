# KOB web app

Static, client-side, non-custodial order book UI. TypeScript + Preact on top of `kob-wasm` (our Rust `kob-protocol`: order builders,
signing split build -> digests -> finalize, fees, budgets, KOB1 codec, state codec) and the official kaspa-wasm v2.1.0 SDK (wRPC, addresses).
TypeScript is only UI and wallet glue: no protocol logic is re-implemented. Wallets: KasWare, Kaspire, Kastle (behind `features.kastle`).
Data: the indexer REST/WebSocket API (`crates/kob-executor`, `docs/ops/indexer.md`) and node RPC.

## Setup

```
cd web
npm ci
npm run fetch-sdk            # official rusty-kaspa v2.1.0 wasm SDK -> vendor/ (the npm "kaspa-wasm" package is stale, do not use it)
npm run build:wasm           # scripts/build-wasm.sh --web at the repo root (needs the wasm32 target and wasm-bindgen-cli 0.2.100), copies to wasm/
npm run typecheck && npm test
npm run dev                  # or: npm run build   (static output in dist/)
npm run build:release        # release: build + ship the pinned registry as dist/registry/tokens.json (docs/ops/release.md)
```

Configuration (`config.json` next to `index.html`, `window.__KOB_CONFIG__`, Settings screen): `network`, `indexerUrl`, `nodeUrl` (`ws://` wRPC node,
empty = the SDK's public resolver), `registryUrl` (default `./registry/tokens.json`), `features.kastle`, `fees` (`dynamic`, `maxRate`, `maxFeeKas`: see Fees). URL query overrides are off unless
`allowQueryOverrides` is set.

Transaction links: a recent-trades row, an order's placement transaction and its fill rows open the transaction on the block explorer in a new tab (`https://tn10.kaspa.stream/transactions/<txid>` on testnet-10, `https://kaspa.stream/transactions/<txid>` on mainnet); `explorerUrl` (config.json / `__KOB_CONFIG__` only) replaces the base.

The landing screen and the USD reference token: `quoteTokens` (config.json / `__KOB_CONFIG__` only, `{ "<covenant id>": "USD" }`) marks a registry token as the USD
reference (the TN10 soak's TUSD, 1 TUSD = 1 USD worth of KAS). It only decides the default orientation of that token's market (stablecoin as quote: KAS/TUSD,
never TUSD/KAS) and the landing screen; tickers are never renamed. There is ONE market page with one layout (title bar, 24 h strip, then order book | chart + depth + trades | ticket): `#/market/<id>` is the token's KAS market
(TOKEN/KAS, with a flip control) and `#/market/<base>/<quote>` a token pair (BASE/QUOTE through the two KAS books, e.g. TBTC/TUSD; the old `#/pair/<base>/<quote>`
is an alias). The title shows the pair as `BASE / QUOTE`, each side a drop-down of KAS and the tokens (a token pair needs both tokens in the registry): choosing
another asset changes the market in place, the URL follows (browser back works), nothing else moves. Both are tradable. Old `#/usd/kas` and `#/usd/<token>` links are redirected (history entry replaced) to the USD token's market and the pair
`<token>/<USD token>` (the market list without a USD token). The pair chart converts candles bucket by bucket on one time grid, so each bucket uses the
time-aligned price of both legs (method in `src/ui/market/usd-model.ts`). `home` picks the landing screen (empty hash): `auto` (default: the USD token's market, shown KAS/<ticker>, else the
chart of the first tradable token, else the list), `list`, `kas-usd` (the USD token's market), `usd:<covenant id>` (the pair `<id>/<USD token>`), `market:<covenant id>`; the market list is always `#/market` (header "Market"). Every token market is shown as TOKEN/KAS and a USD reference token as KAS/<its ticker>; each market has an icon-only flip button ("Flip pair", just left of the pair title) that inverts the display (price, chart, book, trades, stats and the ticket; a flip never changes the order that is built), remembered per market in localStorage (`kob.flip.v1:<market>`). The flip is complete: the book has its own price grouping in the shown unit (steps of the displayed price), its notes, the depth tooltip and the trade-amount column (fixed decimals from the market's tick) follow the orientation, and the ticket follows the shown pair as BASE/QUOTE: in KAS/TOKEN the base is KAS, so Buy / Sell are KAS's, prices are tokens per KAS and every amount box counts KAS, converted into the token at the price of its own leg (the limit; a stop; an exit price; the book's best price for market and close, flagged approximate) and rounded down, with the exact token amount shown under the box; the helps and type names follow the displayed Buy / Sell and the tip is KAS per token traded. The disclosure headline and a line at the top of the confirmation screen state the order in the shown pair's words (what is given and received); the decoded transaction below it stays in native terms (it shows the order exactly as it is built). A pair page flips by opening B/A and carries the order typed there across: the side turns over, amounts and prices are converted at the order's price (`ui/ticket/pair-flip.ts`).

## Layout

| Path | Contents |
|---|---|
| `src/kob/` | pure protocol-side logic over kob-wasm: order planners for every order type (`orders/`, `intent-*.ts`, `plan.ts`; cross limits `orders/cross*.ts`, pair math `pair.ts`), pre-sign decoding (`decode.ts`), token registry (`registry.ts`), placement records and recovery, cancel / cancel-replace / cancel-all, balances, positions, issuance |
| `src/data/`, `src/wallet/` | node RPC (official SDK) and mock-node client, indexer client + WebSocket feed, UTXO services, token tracker, wallet adapters, sign-and-submit pipeline |
| `src/app/`, `src/ui/`, `src/i18n/` | composition root, views (market, ticket, confirmation, my orders, issuance, settings), English text |
| `mock/`, `e2e/` | mock indexer + node server and fake wallets; Playwright specs for every order type (`e2e/README.md`) |
| `e2e-real/` | real-wallet run on TN10 with KasWare and Kaspire (`e2e-real/README.md`), skipped offline |

## Market page

`#/market/<token>` is an exchange layout: title bar and 24 h stats (`GET /v1/stats`), then order book | price chart, depth chart and trade tape |
order ticket and balances (3 columns from 1200 px, 2 on tablets, stacked on phones without horizontal scroll), token facts below.

* **Data.** `GET /v1/trades|candles|stats|depth/{token}` (`src/data/indexer.ts`, contract in the executor docs). Prices are sompi per `price_basis`
  token base units; KAS per whole token = `price / 1e8 * 10^decimals / price_basis` (`src/ui/market/market-model.ts`, exact bigint text, floats only for
  chart coordinates). An indexer without these routes (404 / 405 / 501) degrades per panel: the tape falls back to `/v1/fills`, depth to the book,
  stats and candles show "no data".
* **Chart.** Candles 1m / 5m / 1h / 1d (remembered) with a volume histogram; empty buckets carry the previous close (flat, muted, zero volume).
  The chart library is loaded on demand (its own ~63 kB gzip chunk).
* **Book.** Aggregated levels with depth bars, the spread row with the last price, price grouping 1x / 10x / 100x / 1000x the tick (asks round up,
  bids down; a click on a grouped row prefills the worst price of the group).
* **Live.** The WebSocket channels `book:<token>` / `fills:<token>` trigger a refetch of every panel (debounced 1 s); 5 s polling while the socket is down.
* **The book never disappears.** A failed or late book pull keeps the last good book on screen with a subtle `stale since hh:mm:ss` marker (the time of that book); only the next good response replaces it. A crossed book (matchers a moment behind) is the live book like any other: it is drawn as it is, with a negative spread (`Mid 0.043173 · Spread -0.000052 (-0.11%)`), and is not marked stale. A pull in flight is never aborted by the next refresh (a live notice or a poll that arrives meanwhile runs once right after it). The indexer-lag banner and the book's "possibly out of date" badge show only when the indexer is more than 30 s behind the chain.
* **Theme.** Dark / light toggle in the header, remembered in localStorage (`kob.theme`), default = the OS preference (dark when unknown).
* **Try it.** `npm run mock -- --history --fund` (mock indexer + node on :8790 with a seeded 48 h price history), `npm run dev`, then Settings:
  indexer and node URL `http://127.0.0.1:8790`. `KOB_SHOTS=1 npx playwright test e2e/ui/market-data.spec.ts` saves screenshots to `test-results/shots/`.

### Chart library license

[TradingView Lightweight Charts](https://github.com/tradingview/lightweight-charts) (npm `lightweight-charts` 5.x) is **Apache-2.0**. Its README adds an
attribution requirement: the NOTICE ("TradingView Lightweight Charts™ Copyright (c) 2025 TradingView, Inc. https://www.tradingview.com/") and a link to
https://www.tradingview.com/ on the pages that use it. We keep the library's default `attributionLogo` on the chart (a link to TradingView, which the
README says satisfies the link requirement) and show the notice with the link in the footer (`src/ui/shell/Footer.tsx`). Do not remove either.
Its only dependency, `fancy-canvas`, is MIT.

## Token/token pairs and cross limits

There is no token/token book: a pair BASE/QUOTE (e.g. BTC/USDT) settles through the BASE/KAS and QUOTE/KAS books in ONE transaction. `#/market/<base>/<quote>`
(reached from the quote side of the title's `BASE / QUOTE` selector; both tokens must be in the registry) shows the implied book of `GET /v1/pairs/{base}/{quote}/book`
and the pair ticket (`src/ui/market/PairPage.tsx`, `CrossTicket.tsx`, `pair-model.ts`).

* **Prices.** Exact rationals: the indexer quotes `price_num / price_den` QUOTE base units per BASE base unit; the UI shows QUOTE per WHOLE BASE
  = num/den x 10^(baseDecimals - quoteDecimals) as exact decimal text (asks rounded up, bids down; floats only for depth bars; `src/kob/pair.ts`).
  **Direct** levels are resting cross limits of the pair, **route** levels (labelled "via KAS") are quotes implied through the two KAS books. Live
  refresh on `book:<base>` / `book:<quote>`; an indexer without the pair routes (404 / 405 / 501) shows "pair book unavailable" and the ticket still works.
* **Cross limit** (`KobCross` / `KobCrossKron`, `src/kob/orders/cross.ts`). "Sell BASE for QUOTE" is a cross order A = BASE -> B = QUOTE, "Buy BASE
  with QUOTE" one A = QUOTE -> B = BASE. The amount is whole lots of the SOLD token (its registry `lot_size`, written as `lotUnits = 1, unit = lot_size`).
  The price becomes `bLot` = the RECEIVED base units per lot (at least), rounded UP (sell: `ceil(price x lot x 10^qd / 10^bd)`, buy: `ceil(lot x 10^bd /
  (price x 10^qd))`), so every fill pays at least the entered price; the ticket shows the effective rate, the amount per lot and the minimum
  received. The optional tip is typed as on the KAS ticket (KAS per whole token of the sold token) and prefunded in the order per lot. Time in force: GTC / day / GTD, IOC, FOK. Refund tip = the sold token program's default
  (kob-wasm `keeperTips`).
* **The pair ticket is the KAS ticket.** Same order types by name (Limit, Market, IOC, FOK: `PAIR_TYPES` in `cross-form.ts`), same fields, labels and wording (`ticket.*` keys), no
  cross-only control: the amount is rounded to whole lots of the received token and the number of prefunded deliveries is chosen by the form, both silently. The KAS ticket's
  streaming, close, stop, trailing, take-profit, OCO, if-done, repeat, TWAP, DCA and Dutch types are hidden on a pair (`PAIR_HIDDEN_TYPES`): a cross order cannot be them.
* **Carriers.** `deliveryCarrier` = the covenant carrier (2 KAS by default, like a bid's). A resting order prefunds one delivery per partial fill it can take,
  `min(lots - 1, 3)` (the bid default of 3 fills): order value = carrier + deliveries x deliveryCarrier; an IOC / FOK order takes one fill that moves
  the whole order value onto the delivery, so its value is the carrier. The value is at least deliveryCarrier + 1 sompi (protocol minimum; a 1-lot or IOC
  order gets that). Unused carriers come back with the last fill, a refund or a cancel (disclosed).
* **Market order on a pair** (`src/kob/orders/cross-market.ts`): an IOC cross limit AUCTION (protocol v2.6: `bLotEnd`, `auctionDaa`), filled by a matcher
  through the KAS route (A -> KAS -> B in one transaction). The guaranteed rate starts at what the route pays for the order in whole lots of the received token (`bStartLot`: at most the best level, rounded
  down; a matcher delivers whole ask lots, so a higher start only delays the first possible fill; `CROSS_WHOLE_LOTS_BELOW_BOUND` when a size cannot fill)
  and relaxes linearly to the worst bound over 20 s (`MARKET_AUCTION_DAA`), then stays there until the IOC kill; activation and life are the plain market
  order's (+30 DAA, 300 DAA). The worst bound is the worst ROUTE level the amount needs (direct levels are not route liquidity) minus / plus the slippage
  bound (default 3%); the maker receives exactly `n x (rate(t) - tipLot)` at the fill's time `t`, so matcher competition improves the price. The ticket
  shows the expected average, the start and the worst rate ("fills between {start} and {worst}; better if matchers compete"); resting cross limits keep a
  fixed rate. The app signs no taker (`swapRoute`) transaction.
* **Confirmation screen.** `decodeSigning` describes a cross limit from the state alone (pair, lots, guaranteed rate per lot and per whole token (an auction: the start-to-worst range and its window),
  minimum received at the worst rate, tip, time in force, delivery carrier). A token B outside the registry is a warning; a B pinned to another program, family or
  extension than its registry entry, a `bFamily` its program contradicts, or a rate the covenant refuses (at or below the tip, a fixed rate with an end, a malformed or overflowing auction) is blocking. A cancel sweeps the strays of
  BOTH tokens (each in its own transfer; `cross-strays-swept`); cancel-all cancels every cross limit in a transaction of its own.
* **My orders** lists cross limits with the pair (link to the pair view), the guaranteed rate, filled / left and status; cancel and refund work as for asks.
* **Mock.** `npm run mock -- --pair` (or `POST /mock/seed {pair: true}`) adds EXUSD (6 decimals) with a KAS book and resting cross limits on EXKCC/EXUSD;
  `mock/pairs.mjs` serves both pair endpoints. The e2e registry patch lists EXUSD (`e2e/ui/pair.spec.ts`).

## My orders: amend, close, fills, notifications

* **Amend.** A plain ask keeping its lots, and a plain bid keeping its token and lot, are amended IN PLACE (one `AMEND` record: same
  order id, the ask's custody never moves, the bid's escrow pays the fee or is topped up). Stops / take-profit / OCO have a quick form
  (prices, lots, tip, the stop's worst price) and, like trailing stops and IFD / IFO entries, the full replace form: the order is turned back
  into its ticket intent (`src/ui/orders/replace-intent.ts`), edited in the ticket's own fields (band, trigger rule, trailing, expiry,
  activation) and re-planned as ONE cancel-replace transaction. A resting cross limit amends its rate, lots and tip. Repeat entries and booked
  exits are cancelled as a position (their exits re-arm the entry). A GTC replacement gets a fresh 90 days.
* **Close.** Buy first: cancel the position's orders, then sell the released tokens at market (`?ticket=close`). Sell first: cancel
  them (their KAS comes back), then buy back the lots sold and not yet bought back at market (`?ticket=cover&lots=N`).
* **Fills.** Per order on demand (time, lots, price, payout, average) and every fill of the wallet as one CSV (`src/ui/orders/fills-model.ts`).
* **Notifications** (opt-in, client side only): fills, partial fills, stops armed / triggered, expiry soon, expired / refunded / killed,
  incoming payments and watched x402 invoices; in-app list and toasts, optionally browser notifications (`src/kob/notifications.ts`,
  `src/ui/notify/`).
* **Automatic refund** (opt-in): while the app is open, expired own orders are refunded with the covenant's signature-free `refund()`
  (`src/ui/orders/auto-refund.ts`); a refund that would need a funding signature is left to the Refund button.
* **Covenant signing check.** A wallet not known to sign covenant inputs (every cancel needs one; Kastle builds with issue #353 do not)
  signs one test cancel of a synthetic order, never broadcast, before its first order (`src/wallet/covenant-probe.ts`).
* **Records.** When the browser refuses to store the placement records they live in the tab only: My orders says so and offers the backup.
* **Strays (sweep in place).** Token UTXOs sent to an order from outside are listed per order and in the strays panel (foreign ones, of another token,
  flagged; a token outside the registry marked unknown). "Sweep strays" (`planSweep`, kob-wasm `sweepOrder`, KOB1 `SWEEP` record) returns them while
  the order lives on: the maker's cancel continues the order under the SAME script (same price, lots, custody; its 90-day idle window restarts), one
  token output per token (own token within one extension commitment and the program's inputs, a cross limit's B, foreign strays with a proven state);
  the rest stays for a later sweep. A plain ask's carrier pays the fee, every other kind one wallet coin. The confirmation screen (`decodeSigning`,
  kind `sweep`) re-checks the record (`src/kob/sweep.ts`, as `verify_sweep`) and blocks another script, a record that does not verify, the swept
  order's custody being spent or any token to another key. A cancel that would abandon strays (more than one transaction can move) offers "Sweep
  first". Cancels return foreign strays too. The placement record stays and points at the continuation.

## Wallet events

After connect the adapters listen for account and network changes (`WalletAdapter.subscribe`: the providers' `accountsChanged` / `networkChanged`, which KasWare, Kaspire and Kastle all emit through `on` / `removeListener`; a visible-tab poll if a provider has no emitter). An event is only a trigger: the app re-reads the wallet silently (`refresh`, no popup) and reacts (`src/app/wallet-events.ts`): a new key moves the session, balances and orders to it and resets state bound to the old one; another network shows the mismatch banner and blocks planning/signing until the wallet is back; a wallet without an account closes the session. The confirmation screen refuses a plan whose signer key is not the wallet's, and `signAndSubmit` re-reads the wallet right before asking it to sign.

## Trust boundaries

* **Node over indexer.** The KAS value of a covenant / token input (custody UTXO, strays) is committed by no signature, so it is never taken from the
  indexer: cancel / amend / cancel-all re-read the order, custody and strays from the NODE (`src/kob/node-verify.ts`, used by `snapshotFor`); an
  indexer answer the node does not confirm falls back to the node-resolved placement record, else nothing is built. The confirmation screen re-reads
  every input of the transaction from the node too and `decodeSigning` blocks any input whose amount / script / covenant id differs
  (`input-unconfirmed`); `signAndSubmit` repeats that check right before the wallet popup and compares the txid the node returns with the signed one.
* **Trust badges come from the shipped registry.** `official` / `verified` and the freeze / seize / blacklist warnings are read from the bundled registry
  (`official`, template `capabilities`); the indexer's `standing` can only downgrade (delisted / unverified) and its `powers` can only add. A registry
  token also shows `genesis verified` / `genesis not verified` (registry field `genesis_verified`, absent = not verified) and an optional `warning`.
* **Official = listed in the registry this build pins.** The app hashes (sha256, exact bytes) the registry it loaded and compares it with the hash of
  `registry/tokens.json` pinned at build time (`registry-pin.mjs`, vite `define`). Any other list (a `registryUrl` override, an edited file, a third-party build)
  is a **non-default registry**: a persistent banner, no token labelled official / verified (they read "listed in custom registry <hash>"), and the identity
  (source, network, sha256, counts) is shown in the footer, in Settings ("Trust sources"), on the token page and in the ticket. Updating the registry file of a
  deployment therefore requires rebuilding the app (the pin moves with the file).
* **Several indexers.** `extraIndexerUrls` (config.json / `__KOB_CONFIG__`, at most 4) are cross-checked on the token page: standing, powers and best ask / bid are
  compared and any disagreement is shown. With one indexer the footer and Settings say "single indexer, unverified".
* **Guards.** If the indexer cannot list the wallet's own orders (self-trade prevention) or the book, the ticket says so and blocks until the user
  acknowledges. Market orders show their reference price source and are cross-checked with the last fill.

## Deployment headers

The build embeds a strict Content-Security-Policy as a `<meta>` (`csp.mjs`); `public/_headers` (Netlify / Cloudflare Pages format) sends the same policy as a
real header plus `frame-ancestors 'none'` (a `<meta>` cannot express it), `X-Content-Type-Options`, `Referrer-Policy`, `X-Frame-Options`. Other hosts must set the
same headers (nginx: `add_header Content-Security-Policy "..." always;`, value = `CSP_HEADER` of `csp.mjs`). The node and indexer are user-configurable, so the
shipped `connect-src` only names schemes (`'self' https: wss: http: ws:`): **a production deployment should replace it with the exact origins** of its
node and indexer, e.g. `connect-src 'self' https://indexer.example wss://node.example:18210`. The SDK zip that `npm run fetch-sdk` downloads is checked against a pinned
sha256 (`scripts/fetch-sdk.mjs`); a mismatch aborts before anything is unpacked.

## Tests

* `npm test`: unit tests (vitest), including consensus checks: every planned transaction is signed locally, finalized and validated in the wasm script engine.
* `npm run build && npm run e2e`: mock-API + mock-wallet Playwright suite (needs Playwright's Chromium: `npx playwright install chromium`).
* `node e2e-real/run.mjs`: real wallets on TN10 (needs extensions, funded DEV key in the gitignored `e2e-real/.env`, a TN10 node of your own: `KOB_TN10_WRPC`, default `ws://127.0.0.1:18210`).

## Indexer, token families, fees

* **Token holdings.** `GET /v1/token-utxos?owner=&token=&spent=` (`docs/ops/executor.md` 5.1: proven states of KCC-20 and KRON holdings, paged) is the first source of the
  wallet's token UTXOs (`src/data/token-tracker.ts`). Every UTXO is still verified on the node (its P2SH is derived from the claimed state) before it is spent. The
  indexer cannot list issuance (genesis) outputs and may lag a block, so the local tracker (outputs of the app's own transactions, manual import) stays as the
  complement; an outpoint the indexer lists is dropped from the local list. An indexer without the endpoint (404 / 501) degrades to the local tracker.
* **One lot convention.** `lot_size` is the standard lot in TOKEN BASE UNITS in the registry, in the indexer allowlist and in `GET /v1/tokens`; an order is listed only when
  `lotUnits * unit == lot_size`. The app writes `lotUnits = 1, unit = lot_size` (price of a state = sompi per lot); other clients may split the lot differently, so every
  book level and order row carries its own `lot_units` and the app converts to per-lot prices row by row (equal per-lot prices merge).
* **Token families.** KCC-20 and KRON tokens trade through the same planners: the family follows the registry template (`kcc20-*` / `kron-*`), order states are tagged
  `KobAsk` / `KobAskKron` etc. (`src/kob/order-facts.ts`: `baseKind`, `kindFor`), token states are `Kcc20State | KronState` (`src/kob/token-state.ts`; a wallet's KRON tokens are
  address-presence UTXOs, `id_type` 3, authorised by a P2PK funding input, never by a signature of their own). Issuance is KCC-20 only.
* **Fees.** A transaction pays `rate x max(compute, normalized transient)` (storage mass is not charged). The RATE is the dynamic fee policy (`src/kob/fee-policy.ts`, the same
  policy as the Rust executor): the node's `getFeeEstimate` bucket of the action's urgency, clamped to [100 sompi per gram (the relay floor), `fees.maxRate` (default 1000)], with a once-only
  rebuild at a lower rate when the fee would exceed `fees.maxFeeKas` (default 1 KAS per transaction; 0 = no cap). Urgency: HIGH = IOC / FOK / market / streaming / close, a limit that
  crosses the book now and cross IOC / FOK (priority bucket, sub-second); NORMAL = resting orders, schedules, stops, if-done, cancels, amends, refunds, issuance (first normal bucket,
  sub-minute); LOW = housekeeping (first low bucket). No usable estimate (call failed, method missing, malformed, older than 60 s), `fees.dynamic: false`, or a wallet that cannot afford
  the picked rate = the floor, never an error. The confirmation screen (and the ticket's disclosure) shows the rate, its bucket and expected time, and a note for every fallback or cap.
  `fees.dynamic` is also a Settings switch. The mock server answers `GET /node/fee-estimate` once `POST /mock/fee-estimate {priority, normal, low}` set it (404 otherwise = floor).
  Independently, Settings > "Pay the priority fee" (`features.priorityFee`) switches every planner to the storage-inclusive fee MODE (`feeMode: 'priority'`); the confirmation
  screen's technical details show the mode and both masses. Mode and rate are independent.
* **Order views** need `state`, `current`, `custody.utxo`, `strays`, the position grouping needs `parent` / `repeat` (indexer available; without it My orders shows node-derived rows).
