# KOB web app

Static, client-side, non-custodial order book UI. TypeScript + Preact on top of `kob-wasm` (our Rust `kob-protocol`: order builders,
signing split build -> digests -> finalize, fees, budgets, KOB1 codec, state codec) and the official kaspa-wasm v2.1.0 SDK (wRPC, addresses).
TypeScript is only UI and wallet glue: no protocol logic is re-implemented. Wallets: KasWare, Kaspire, Kastle (behind `features.kastle`).
Data: the indexer REST/WebSocket API (`crates/kob-executor`, `docs/ops/executor.md` Part B) and node RPC.

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
empty = the SDK's public resolver; an `http(s)://` URL is the offline mock node and only a test build (`features.test`) or `npm run dev` accepts it), `registryUrl` (default `./registry/tokens.json`), `features.kastle`, `fees` (`dynamic`, `maxRate`, `maxFeeKas`: see Fees). URL query overrides are off unless
`allowQueryOverrides` is set.

Token names and icons: the market list (and a token page's title bar) is completed from a kascov.io-shaped data set (`<tokenNamesUrl>/<network>/tokens.json`, default `https://kascov.io/data`; `tokenNamesUrl` is config.json / `__KOB_CONFIG__` only, https only, `""` turns it off): tokens of a program this build trades on the network that no order names, and the creators' claimed ticker, name and icon where KOB has none, all labelled "declared by its creator". Opening such a token (no order of it yet) needs no wallet holding: its page takes the token program from the template hash this build pins, and the extension commitment from token outputs of it that the indexer names (`GET /v1/token-utxos?token=`) and the node confirms (`findPublicToken`); the decimals follow the usual rules (declaration, the program default such as KaspaRocket 3, else the first order names them). Where a fact cannot be verified the page says exactly which, with no dead end. KOB's own ticker and decimals win; no fact comes from it (`src/data/kascov.ts`, `src/ui/market/kascov-merge.ts`, `docs/ops/web-hosting.md`). A picture is shown only if its bytes hash to the hash the creator committed to, from a `blob:` URL (CSP `img-src 'self' data: blob:`); the Settings switch "Load token icons from third-party hosts" (`kob.icons`, on by default) turns pictures off.

Transaction links: a recent-trades row, an order's placement transaction and its fill rows open the transaction on the block explorer in a new tab (`https://tn10.kaspa.stream/transactions/<txid>` on testnet-10, `https://kaspa.stream/transactions/<txid>` on mainnet); `explorerUrl` (config.json / `__KOB_CONFIG__` only) replaces the base.

The landing screen and the USD reference token: `quoteTokens` (config.json / `__KOB_CONFIG__` only, `{ "<covenant id>": "USD" }`) marks a registry token as the USD
reference (the TN10 soak's TUSD, 1 TUSD = 1 USD worth of KAS). It only decides the landing screen; its market opens as TUSD/KAS like every other market (the token on the left, amounts
in the token) and the flip shows KAS/TUSD (amounts in KAS); tickers are never renamed. There is ONE market page with one layout (title bar, 24 h strip, then order book | chart + depth + trades | ticket): `#/market/<id>` is the token's KAS market
(TOKEN/KAS, with a flip control) and `#/market/<base>/<quote>` a token pair (BASE/QUOTE through the two KAS books, e.g. TBTC/TUSD; the old `#/pair/<base>/<quote>`
is an alias). The title shows the pair as `BASE / QUOTE`, each side a drop-down of KAS and the tokens (a token pair needs both tokens in the registry): choosing
another asset changes the market in place, the URL follows (browser back works), nothing else moves. Both are tradable. Old `#/usd/kas` and `#/usd/<token>` links are redirected (history entry replaced) to the USD token's market and the pair
`<token>/<USD token>` (the market list without a USD token). The pair chart converts candles bucket by bucket on one time grid, so each bucket uses the
time-aligned price of both legs (method in `src/ui/market/usd-model.ts`). `home` picks the landing screen (empty hash): `auto` (default: the USD token's market, shown KAS/<ticker>, else the
chart of the first tradable token, else the list), `list`, `kas-usd` (the USD token's market), `usd:<covenant id>` (the pair `<id>/<USD token>`), `market:<covenant id>`; the market list is always `#/market` (header "Market"). The list shows each tradable token's KAS market (TOKEN/KAS) with its last price, 24 h change, 24 h KAS volume, spread (best bid / ask in the tooltip) and open orders from `/v1/stats/<token>` (one call per token, four in flight), and below it the token pairs with live pair orders (`/v1/pairs`; price and change implied through the two KAS markets as on the pair page, 24 h pair volume from `/v1/pairs/<base>/<quote>/fills`); every column header sorts (24 h volume first, missing figures last), the search also matches pairs (`BASE/QUOTE` too), a figure the indexer cannot give reads `—`, and a phone shows each token as a card (identity and text left, price over change right; `src/ui/market/market.css`). Each row leads with an identity block: the token icon and a fingerprint of its covenant id (`src/app/fingerprint.ts`: Hallmarks v1.0, a 5x7 dot pattern and three BIP-39 words from the SHA-256 of the lowercase id; vendored data and conformance vectors in `src/third-party/hallmarks/`; the words are the picture's tooltip and accessible name), then the ticker (bold) and name with a small "via kascov" marker where the name came from kascov.io, then the covenant id shortened (8 + 8 hex) with a copy button and the badges; one legend above the table explains the picture, "via kascov" and the badges. The token and pair page headers show the same block at 64 px. Sorting and the figures: `src/ui/market/market-list.ts`. Every token market, a USD reference token included, opens as TOKEN/KAS; each market has an icon-only flip button ("Flip pair", just left of the pair title) that inverts the display (price, chart, book, trades, stats and the ticket; a flip never changes the order that is built), remembered per market in localStorage (`kob.flip.v2:<market>`). Book, pair book and tape columns never cut a digit: a column too wide for its cells shows fewer decimals or K / M / B (`56.61M`, `<0.01`) with the exact amount in the tooltip (`fitAmountColumn`). The flip is complete: the book has its own price grouping in the shown unit (steps of the displayed price), its notes, the depth tooltip and the trade-amount column (fixed decimals from the market's tick) follow the orientation, and the ticket follows the shown pair as BASE/QUOTE: in KAS/TOKEN the base is KAS, so Buy / Sell are KAS's, prices are tokens per KAS and every amount box counts KAS, converted into the token at the price of its own leg (the limit; a stop; an exit price; the book's best price for market and close, flagged approximate) and rounded down, with the exact token amount shown under the box; the helps and type names follow the displayed Buy / Sell and the tip is KAS per token traded. The disclosure headline states the order in the shown pair's words (what is given and received); the transaction built is the native order. A pair page flips by opening B/A and carries the order typed there across: the side turns over, amounts and prices are converted at the order's price (`ui/ticket/pair-flip.ts`).

## Layout

| Path | Contents |
|---|---|
| `src/kob/` | pure protocol-side logic over kob-wasm: order planners for every order type (`orders/`, `intent-*.ts`, `plan.ts`; pair orders `orders/pair-*.ts`, pair math `pair.ts`), pre-sign decoding (`decode.ts`), token registry (`registry.ts`), placement records and recovery, cancel / cancel-replace / cancel-all, balances, positions, issuance |
| `src/data/`, `src/wallet/` | node RPC (official SDK) and mock-node client, indexer client + WebSocket feed, UTXO services, token tracker, wallet adapters, sign-and-submit pipeline |
| `src/app/`, `src/ui/`, `src/i18n/` | composition root, views (market, ticket and its send flow, my orders, issuance and its confirmation, settings), English text |
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
* **Order button.** The ticket holds only the inputs (the amount box shows the balance it spends, "Available 988.41 KAS", a tap is Max; on
  the KAS/TOKEN view one line converts the KAS) and one button ("Buy Limit", "Sell Market order" ...). The button plans the order again from
  the state at the click with the node clock (read every 2 s in the background, projected to the click) and runs the pre-sign checks silently
  (`src/ui/confirm/send.ts`: wallet account and network, every input re-read on the node, the built transaction decoded and compared with the
  plan, the re-check against the state at the click: a moved book is the order to sign as long as it keeps the terms shown); only a check that
  makes the order wrong or impossible stops it, with one line. Then the final confirmation, the one place with the order's details: the
  private-key signer's table, decoded from the transaction (`src/signer/summary.ts`: the net effect on the wallet, what is locked in
  orders, the network fee, then each order with its token and terms). The signer draws it in its own frame and its Approve signs and
  sends; for an external wallet the app shows the same table and its "Continue in wallet" hands the transaction to the wallet, whose own
  popup follows. An auction whose start passed while the user read it is re-timed (the same plan, later times) and signed again without a
  second prompt when the signer's own decoding finds nothing worse (`src/signer/retime.ts`). Its new start leaves at least as long as
  the failed signature took, and a wallet whose last auction signature took longer than the 5 s window (30 DAA activation lead plus
  20 DAA margin) gets its next auction planned with a start later by the shortfall plus 3 s, at most 60 s
  (`src/ui/confirm/signing-time.ts`): only times move, later.
  While it is open the confirmation follows the market and the clock: the order is planned again every 1.5 s (`SendJob.replan`), checked
  as strictly as the first and put in its place, so what is approved is what is shown; the signer frame checks each update itself (an
  update worse for the user than what was shown says so and holds Approve for half a second), changed figures flash, and the start and
  expiry read "in 28 s" over their DAA score. The signer frame is as tall as its content (it reports its height), up to the viewport.
  One status line under the button follows it all (checking, confirm, approve in your wallet, sending, the
  txid with its explorer link, accepted). No checkboxes. The planner's warnings that matter to a trader (a price far through the market,
  a book that disagrees with the last fill, a high slippage, a day order about to end, a limit that crosses the market, the conditional orders'
  market checks, a stop on a token whose genesis is not verified) are one-line notes next to the button (`src/ui/ticket/ticket-notes.ts`): they
  never stop the order and need no click; only errors disable the button. An auction (market, marketable limit) starts from the clock of its plan: confirmed in
  the app's popup more than 1 s after it opened, it is planned again with the current clock before the wallet gets it; a signature that comes
  back more than `AUCTION_START_MARGIN_DAA` (20 DAA, 2 s) after the start is not sent, the order is planned again and the wallet asked once
  more (`src/wallet/sign.ts`). Neither asks the user again when nothing they confirmed got worse (`src/signer/retime.ts`: the private-key
  signer verifies that from its own decoding of both transactions), else the updated confirmation is shown once more. My orders actions
  (cancel, amend, refund, sweep, merge) run the same way; their balances (tokens in orders, KAS locked) are at the top of My orders. Token
  issuance keeps its confirmation screen (a token's facts never change).
* **Ladder.** The protocol has no ladder order: a ladder is N ordinary orders, each its own covenant, transaction and signature, planned and
  signed one after the other (`src/ui/ticket/ladder.ts`). A limit order has a "Ladder" switch (2 to 20 orders, "Step between orders" in percent
  or "Or end price"; a buy ladder goes down from the typed price, a sell ladder up); a repeat IFD / IFO has "Ladder levels" (1 to 20) with a
  "Ladder step" that shifts the entry and the exit prices together. The ticket lists the levels and the total locked before the first
  signature. Stopping keeps the levels already placed ("Stopped: N of M levels placed"); nothing is batched and a half-placed ladder is not
  remembered across a reload, so the rest has to be placed again by hand (the placed ones are in My orders). With the private-key signer every
  level has its own Approve.
* **Fills of an order that takes liquidity.** One transaction takes at most the token's input slots of asks (3 on KOB's standard program; a
  sell at most its output slots of bids, one less while it leaves a rest), so a large market buy against many small asks fills only part in
  one transaction. A market order has a Fills box (Advanced; both sides of a KAS market, not with all-or-nothing, not on a pair: pair market
  orders stay one transaction): 1 (default) is the one IOC transaction as before; N > 1 places the same auction (start, bound, slope,
  activation) as a short-lived RESTING order (tif 0, expiry `activeFrom + max(life, auction + 100 DAA)`, 300 DAA by default) that keeps trading
  within the bound across up to N transactions (`src/kob/orders/simple.ts` `planAuction`); a buy deposits a delivery carrier per fill
  (unused ones return with the refund or a cancel, used ones ride on the delivered token UTXOs until merged or sold), a sell none; its
  minimum fill is the smaller of the wallet default and the smallest crossing counterparty, so no fill of a few small ones is blocked; what is
  left at the expiry is refunded by anyone (refund tip) or cancelled. A buy that crosses the book (a limit buy, a rising bid) gets as many
  fills by default as the asks it crosses need (at most 10). Next to the Fills box the ticket shows the visible book's estimate (fills needed,
  3 per transaction, the carriers deposited; `src/ui/ticket/fills-note.ts`, `src/kob/guards.ts` `estimateFills`), and under the amount a
  warning when the fills (an IOC's, a one-fill market order's, or a budget the user set) cannot take what the book holds within the bound;
  the confirmation of a multi-fill market order says it fills in up to N transactions, when the rest returns, and the carriers deposited.
* **Theme.** Dark / light toggle in the header, remembered in localStorage (`kob.theme`), default = the OS preference (dark when unknown).
* **Try it.** `npm run mock -- --history --fund` (mock indexer + node on :8790 with a seeded 48 h price history), `npm run dev`, then Settings:
  indexer and node URL `http://127.0.0.1:8790`. `KOB_SHOTS=1 npx playwright test e2e/ui/market-data.spec.ts` saves screenshots to `test-results/shots/`.

### Chart library license

[TradingView Lightweight Charts](https://github.com/tradingview/lightweight-charts) (npm `lightweight-charts` 5.x) is **Apache-2.0**. Its README adds an
attribution requirement: the NOTICE ("TradingView Lightweight Charts™ Copyright (c) 2025 TradingView, Inc. https://www.tradingview.com/") and a link to
https://www.tradingview.com/ on the pages that use it. We keep the library's default `attributionLogo` on the chart (a link to TradingView, which the
README says satisfies the link requirement) and show the notice with the link in the footer (`src/ui/shell/Footer.tsx`). Do not remove either.
Its only dependency, `fancy-canvas`, is MIT.

## Token/token pairs

A pair BASE/QUOTE (e.g. TBTC/TUSD) is traded with pair orders (`KobPair`, `KobCondPair`, `KobIfdPair`: one template each for
both sides and both token families; `docs/spec/order-types.md`, "Pair orders"). A pair order enforces only its own price: a
matcher nets it against opposite pair orders, routes it through the BASE/KAS and QUOTE/KAS books in one transaction, or fills it
from inventory. `#/market/<base>/<quote>` (reached from the quote side of the title's `BASE / QUOTE` selector; both tokens must be
in the registry) shows the pair book, the panels of a token market and the order ticket (`src/ui/market/PairPage.tsx`,
`pair-model.ts`, `pair-market.ts`).

* **Book.** `GET /v1/pairs/{base}/{quote}/book`, three sources drawn apart: `direct` (resting `KobPair` orders), `entry` (if-done
  pair entries resting at their limit) and `via KAS` (quotes implied through the two KAS books). Prices are exact rationals: the
  indexer quotes `price_num / price_den` QUOTE base units per BASE base unit; the UI shows QUOTE per WHOLE BASE as exact decimal
  text (asks rounded up, bids down; floats only for depth bars; `src/kob/pair.ts`). A crossed book is drawn as it is. Live refresh
  on `book:<base>` / `book:<quote>`; an indexer without the pair routes (404 / 405 / 501) shows "pair book unavailable" and the
  ticket still works.
* **Prices come only from KAS-book fills.** The chart is the indexer's pair candles (derived from the two KAS series) or the
  client-side ratio of the two KAS candle series; last / change / high / low are KAS-derived. A pair fill is volume only: the
  fills list has no price column.
* **Ticket.** The same ticket and order types as a KAS market (`planPair`, `src/kob/orders/pair-plan.ts`): limit / IOC / FOK /
  market / streaming / close / TWAP / DCA / Dutch are `KobPair`, stops / trailing / take-profit / OCO are `KobCondPair`, IFD / IFO
  / bracket / repeat are `KobIfdPair` with a `KobCondPair` exit. Amounts are base units of A, prices B base units per whole A,
  tips KAS. A sell holds exactly its amount of A; a buy holds a B escrow; the order UTXO prefunds one delivery carrier per
  budgeted fill and the tip. A pair stop arms from evidence of one of two modes: two KAS-book fills of A and B in the same
  transaction, or a resting `KobPair` filled at or beyond the stop.
* **Pre-sign checks.** `decodeSigning` checks both tokens against the registry and a new order's terms against kob-wasm (what returns of
  A and of B, custodies and strays of both tokens, and the evidence of a pair stop armed or trailed by the transaction) before the wallet is asked.
* **My orders** lists a pair order under the type of the KAS kind it mirrors, with the pair (link to the pair view), its prices
  in B, filled / left and status; cancel and refund work as for the KAS kinds, an amend is always a cancel-replace.
* **Mock.** `npm run mock -- --pair` (or `POST /mock/seed {pair: true}`) adds EXUSD (6 decimals) with a KAS book and resting pair
  orders of every kind on EXKCC/EXUSD; `mock/pairs.mjs` serves the pair endpoints. The e2e registry patch lists EXUSD
  (`e2e/ui/pair.spec.ts`).

## My orders: amend, close, fills, notifications

* **Amend.** A plain ask keeping its scale and amount, and a plain bid keeping its token and scale, are amended IN PLACE (one `AMEND` record: same
  order id, the ask's custody never moves, the bid's escrow pays the fee or is topped up). Stops / take-profit / OCO have a quick form
  (prices, amount, tip, the stop's worst price) and, like trailing stops and IFD / IFO entries, the full replace form: the order is turned back
  into its ticket intent (`src/ui/orders/replace-intent.ts`), edited in the ticket's own fields (band, trigger rule, trailing, expiry,
  activation) and re-planned as ONE cancel-replace transaction (pair orders always take this path). Repeat entries and booked
  exits are cancelled as a position (their exits re-arm the entry). A GTC replacement gets a fresh 90 days.
* **Close.** Buy first: cancel the position's orders, then sell the amount the entry bought and the exits did not sell yet at market
  (`?ticket=sell&amount=N`; never the whole free balance). Sell first: cancel them (their KAS comes back), then buy back the amount sold and
  not yet bought back at market (`?ticket=cover&amount=N`).
* **Fills.** Per order on demand (time, amount, price, payout, average) and every fill of the wallet as one CSV (`src/ui/orders/fills-model.ts`).
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
  the order lives on: the maker's cancel continues the order under the SAME script (same price, amount, custody; its 90-day idle window restarts), one
  token output per token (own token within one extension commitment and the program's inputs, a pair order's other token, foreign strays with a proven state);
  the rest stays for a later sweep. A plain ask's carrier pays the fee, every other kind one wallet coin. The pre-sign checks (`decodeSigning`,
  kind `sweep`) re-check the record (`src/kob/sweep.ts`, as `verify_sweep`) and blocks another script, a record that does not verify, the swept
  order's custody being spent or any token to another key. A cancel that would abandon strays (more than one transaction can move) offers "Sweep
  first". Cancels return foreign strays too. The placement record stays and points at the continuation.
* **Merge tokens.** A token whose free UTXOs exceed what one transaction can spend (`TOKEN_UTXOS_FRAGMENTED`; KOB's standard program takes 3)
  gets "Merge (n)" in My orders > Balances (`src/kob/merge.ts`). One click = ONE transfer to the wallet itself of its smallest UTXOs, up to the
  program's token inputs (3 -> 1), for a wallet and the private-key signer alike: a merge is manual and costs a fee, so the user decides each
  transaction and clicks again to merge more; nothing merges automatically (`planMerge` can still build a chain of links, `maxLinks`; the app
  plans one). Only the wallet's own plain UTXOs of the token (tracker, verified on the node) are taken, never an open order's custody or strays.
  The freed carriers pay the fee (low bucket), a wallet coin only when they cannot (and for KRON). The confirmation states the UTXOs merged and
  the count before / after, the KAS the merge frees (the merged UTXOs' carriers less the one the output keeps, less the fee; each token UTXO
  carries ~2 KAS, so freeing them is the point) and how many mergeable UTXOs remain. The button says about how much the next click frees
  ("Merge (n): about +x KAS", before the fee), Balances shows the KAS on the wallet's token UTXOs as its own line (counted in the total: a buy's
  delivery also carries its price improvement over the escrowed worst price, so it can be far more than the carriers), and the private-key
  signer frame adds "KAS freed from your token UTXOs" (its KAS line counts that KAS as the wallet's already, so a merge alone reads as the fee).

## Wallet events

After connect the adapters listen for account and network changes (`WalletAdapter.subscribe`: the providers' `accountsChanged` / `networkChanged`, which KasWare, Kaspire and Kastle all emit through `on` / `removeListener`; a visible-tab poll if a provider has no emitter). An event is only a trigger: the app re-reads the wallet silently (`refresh`, no popup) and reacts (`src/app/wallet-events.ts`): a new key moves the session, balances and orders to it and resets state bound to the old one; another network shows the mismatch banner and blocks planning/signing until the wallet is back; a wallet without an account closes the session. The pre-sign checks refuse a plan whose signer key is not the wallet's, and `signAndSubmit` re-reads the wallet right before asking it to sign.

## Trust boundaries

* **Node over indexer.** The KAS value of a covenant / token input (custody UTXO, strays) is committed by no signature, so it is never taken from the
  indexer: cancel / amend / cancel-all re-read the order, custody and strays from the NODE (`src/kob/node-verify.ts`, used by `snapshotFor`); an
  indexer answer the node does not confirm falls back to the node-resolved placement record, else nothing is built. The pre-sign checks re-read
  every input of the transaction from the node too and `decodeSigning` blocks any input whose amount / script / covenant id differs
  (`input-unconfirmed`); `signAndSubmit` repeats that check right before the wallet popup and compares the txid the node returns with the signed one.
* **No curation of tokens.** Token issuance is permissionless, so the UI shows no verified / official / pending-review labels, no "genesis verified" badge and
  no registry notes about who listed what. What it does show is technical: the token program family, the freeze / seize warnings (template
  `capabilities` from the registry, plus the indexer's `powers`, which can only add), the full covenant id with a copy button, and an `untradable` reason.
  It also shows facts (matcher.md 1.2a, "wallets MUST show the warnings wherever the token is shown"), as neutral badges with a plain sentence in the
  token header, next to the order button and on the list rows: the indexer's `genesis_unverified` and `live_mint_authority` (the same rule applied to
  the registry entry, so they show without an indexer too) and the program powers mint-authority, public-mint and blacklist
  (`src/ui/market/TokenBadges.tsx` `TokenFacts`; on the token page the sentence is printed, on the ticket it opens on a tap, in a list row it is the badge's tooltip and the legend above the list prints every sentence). A name the registry does not
  list is marked "declared by its creator": the creator's own claim, not checked.
  A token this build cannot trade (its program is not one this build supports and embeds) is not listed, not offered in selectors and has no market
  page; no token is ever refused for what it is (KOB makes no listing decision, the registry is a pinned snapshot of facts and curation belongs to
  the community token lists a user chooses). When two or more tokens the app shows share a ticker (look-alike letters and homoglyphs count as the same), the list row (a small warning sign, the sentence is its tooltip), the token page header, the pair
  facts and the order ticket say "Another token uses this ticker — check the covenant id". The covenant id, not the ticker, is the identity.
* **Several indexers.** `extraIndexerUrls` (config.json / `__KOB_CONFIG__`, at most 4) are cross-checked on the token page: powers, the market's price scale and best ask / bid are
  compared and any disagreement is shown. With one indexer the footer and Settings say "single indexer, not cross-checked".
* **Indexer failover.** The same list is the failover set (`src/data/indexer-failover.ts`, `docs/ops/web-hosting.md`): when the primary is unreachable, failing or not
  ready (the status bar's own health judgement) the book, charts, tokens and orders are read from a healthy further indexer on the same network that reports the
  payload version of this build (`book_complete: true` first; a store that began late is used last and the banner says "History may be incomplete"), under the
  banner "Using backup indexer <host>"; the app switches back after two good probes of the primary. The feed reconnects to the indexer in use, one ticket's
  environment is read from one indexer, and the cross-check continues against the others (the primary only while it answers well). No book is built from the node.
* **Guards.** If the indexer cannot list the wallet's own orders (self-trade prevention) or the book, the order cannot be placed: one error line
  ("Could not read the token's guard data; try again in a moment") until the ticket's background refresh reads them, then it clears by itself.
* **Book against the node.** Every read of the order book for a KAS market (`readMarketPart`, `src/app/book-verify.ts`) checks the first orders of each
  side against the NODE: the indexer's view of each order (decoded state, current outpoint) gives its script, and one batched `getUtxosByAddresses`
  (`kob/node-verify.ts` `confirmSnapshotsOnNode`, the reconciliation cancel and amend use) must find the order UTXO (and an ask's custody) unspent at that
  script, with the KAS value and token amount the book row lists and the side, price and scale the state commits. An order the node does not confirm is
  left out of `PlanEnv.book` (`PlanEnv.bookDropped` names it; a taking order shows the warning `BOOK_ORDERS_NOT_ON_NODE`); when none of the first 3 x 3 orders of a
  side is confirmed, the side is empty. A node that does not answer within 5 s rejects the read with a `NodeError`, so the ticket shows its node banner and plans
  nothing. The check is invisible when everything matches and needs no acknowledgement. Pair books (aggregated levels, no orders) are not covered.
* **Market start.** A market or close order starts at the best price of the indexer book, whose touch is confirmed on the node (above). Its start is compared
  with the best price of every `extraIndexerUrls` indexer (a pair market: the rate the further indexer's two KAS books imply); more than
  `marketStartToleranceBps` (config.json / `__KOB_CONFIG__`, 10 to 5000, default 1000 = 10 %) on the costly side of one is an error: the order
  cannot be placed at that start. The last fill comes from the same indexer as the book, so it is no independent reference: without a further
  indexer (none is configured by default) the start rests on the node check above.
* **Stored settings.** A node, indexer or `extraIndexerUrls` list, or `allowQueryOverrides: true`, saved in this browser's settings is named in a banner
  on every page.

## Deployment headers

Hosting the app on Cloudflare Pages (the `deploy-pages` workflow, the `config.json` it writes, the post-deploy header and hash check, rollback, the
neutrality rules): `docs/ops/web-hosting.md`; a public indexer for it behind Cloudflare Tunnel: `docs/ops/chart-server.md`.

The build embeds a strict Content-Security-Policy as a `<meta>` (`csp.mjs`); `public/_headers` (Netlify / Cloudflare Pages format) sends the same policy as a
real header plus `frame-ancestors 'none'` (a `<meta>` cannot express it), `X-Content-Type-Options`, `Referrer-Policy`, `X-Frame-Options`. Other hosts must set the
same headers (nginx: `add_header Content-Security-Policy "..." always;`, value = `CSP_HEADER` of `csp.mjs`). The node and indexer are user-configurable, so the
shipped `connect-src` only names schemes (`'self' https: wss: http: ws:`): **a production deployment should replace it with the exact origins** of its
node and indexer, e.g. `connect-src 'self' https://indexer.example wss://node.example:18210` (add the token-names source too, `https://kascov.io` by default: `docs/ops/web-hosting.md`). The SDK zip that `npm run fetch-sdk` downloads is checked against a pinned
sha256 (`scripts/fetch-sdk.mjs`); a mismatch aborts before anything is unpacked.

The private-key signer (`signer.html`, see below) is a second page with its own policy (`SIGNER_CSP_HEADER`: no network beyond its own bundle,
`frame-ancestors 'self'`); `public/_headers` detaches the app's policy and `X-Frame-Options: DENY` from it (`! Header`, Cloudflare Pages syntax; on
other hosts give `/signer.html` only the signer policy and `X-Frame-Options: SAMEORIGIN`). It runs sandboxed in an opaque origin, so its scripts,
stylesheet and wasm are cross-origin requests: `/assets/*` must be served with `Access-Control-Allow-Origin: *` (public, static files).

### Private key wallet

"Private key" in the header opens `signer.html` in `<iframe sandbox="allow-scripts">` (no `allow-same-origin`). The key is typed into that frame and
held in its memory only (the SDK's `PrivateKey`; no storage of any kind, the frame has none); a reload, closing the tab, disconnecting or removing the
frame forgets it. The app and the frame exchange only the validated messages of `src/signer/protocol.ts`: the unsigned transaction goes in, the
public key and signatures come out. The frame decodes every request itself with kob-wasm (`decodeSigning`), shows the net effect and the fee, and
signs only after a trusted click on its own Approve button. What this does NOT protect against: a script already running in the app page (XSS, a
malicious extension) can draw a fake key field or a fake signer and collect what the user types there; the frame only keeps a key that was typed
into it out of the page's reach. A browser extension wallet is the recommended setup. The frame says before the key is pasted what it is (64 hex
characters, not a seed phrase), that a reload forgets it and to use a dedicated key with a small balance. A browser without IntersectionObserver v2
(iOS, Firefox) cannot run the frame's visibility check: the option is disabled and the header says why in text, with the way out (an extension wallet
on a desktop, Chrome on Android), and a phone browser is not told to install an extension.

## Tests

* `npm test`: unit tests (vitest), including consensus checks: every planned transaction is signed locally, finalized and validated in the wasm script engine.
* `npm run build && npm run e2e`: mock-API + mock-wallet Playwright suite (needs Playwright's Chromium: `npx playwright install chromium`).
* `node e2e-real/run.mjs`: real wallets on TN10 (needs extensions, funded DEV key in the gitignored `e2e-real/.env`, a TN10 node of your own: `KOB_TN10_WRPC`, default `ws://127.0.0.1:18210`).

## Indexer, token families, fees

* **Token holdings.** `GET /v1/token-utxos?owner=&token=&spent=` (`docs/ops/executor.md` 5.1: proven states of KCC-20 and KRON holdings, paged) is the first source of the
  wallet's token UTXOs (`src/data/token-tracker.ts`). Every UTXO is still verified on the node (its P2SH is derived from the claimed state) before it is spent. A
  genesis built by KOB names the states of its token outputs in its payload (`docs/spec/token-metadata.md` 3), so the indexer lists the issuer's outputs from the
  block of the genesis and the same key finds them in any browser (and `kob place` on any machine). The indexer cannot list the genesis outputs of an older creation
  and may lag a block, so the local tracker (outputs of the app's own transactions, manual import) stays as the complement: Settings keeps its export (the backup of an
  older creation) and **Find a token you created earlier** looks the output up on the node from the token and the amount the creation gave the key. An outpoint the indexer
  lists is kept in the local list too (after an indexer reset it is the only record). An indexer without the endpoint (404 / 501) degrades to the local tracker.
* **Units.** Amounts are token base units and prices sompi per whole token: every order carries its `scale` (`10^decimals` of its
  token, at most `10^9`), and `GET /v1/tokens` returns each token's `scale` (`src/kob/units.ts`). There are no lots and the protocol
  has no price tick: the book's grouping step and column decimals use a display tick, the finest step the visible prices sit on
  (`src/ui/market/book-model.ts`).
* **Token families.** KCC-20 and KRON tokens trade through the same planners: the family follows the registry template (`kcc20-*` / `kron-*`), order states are tagged
  `KobAsk` / `KobAskKron` etc. (`src/kob/order-facts.ts`: `baseKind`, `kindFor`), token states are `Kcc20State | KronState` (`src/kob/token-state.ts`; a wallet's KRON tokens are
  address-presence UTXOs, `id_type` 3, authorised by a P2PK funding input, never by a signature of their own). Issuance is KCC-20 only.
* **Fees.** A transaction pays `rate x max(compute, normalized transient)` (storage mass is not charged). The RATE is the dynamic fee policy (`src/kob/fee-policy.ts`, the same
  policy as the Rust executor): the node's `getFeeEstimate` bucket of the action's urgency, clamped to [100 sompi per gram (the relay floor), `fees.maxRate` (default 1000)], with a once-only
  rebuild at a lower rate when the fee would exceed `fees.maxFeeKas` (default 1 KAS per transaction; 0 = no cap). Urgency: HIGH = IOC / FOK / market / streaming / close, a limit that
  crosses the book now and pair IOC / FOK (priority bucket, sub-second); NORMAL = resting orders, schedules, stops, if-done, cancels, amends, refunds, issuance (first normal bucket,
  sub-minute); LOW = housekeeping (first low bucket). No usable estimate (call failed, method missing, malformed, older than 60 s), `fees.dynamic: false`, or a wallet that cannot afford
  the picked rate = the floor, never an error. The ticket's disclosure shows the rate, its bucket and expected time.
  `fees.dynamic` is also a Settings switch. The mock server answers `GET /node/fee-estimate` with every bucket at the relay floor (100 sompi per gram) after each reset; `POST /mock/fee-estimate {priority, normal, low}` changes it, `{off: true}` makes the route answer 404 (no estimate: the floor).
  Independently, Settings > "Pay the priority fee" (`features.priorityFee`) switches every planner to the storage-inclusive fee MODE (`feeMode: 'priority'`); the issuance
  confirmation screen's technical details show the mode and both masses. Mode and rate are independent.
* **Coin selection** (`src/kob/funding.ts`). KAS funding of a placement: smallest first until covered, as the Kaspa SDK's Generator spends a
  UtxoContext kept ascending by amount (at most 50 inputs; when the smallest 50 do not cover it, the largest first with the fewest inputs, and
  `FUNDING_FRAGMENTED` only when even that needs more). More inputs cost signature mass and fee (and more for an external wallet to show), but every
  placement sweeps the wallet's dust into its change; the first funding input authorises the order genesis (its outpoint binds the covenant id),
  any of the wallet's P2PK UTXOs does. Token inputs of a placement (sell, pair custodies): the covering set as before (the smallest single UTXO
  that suffices, else the largest first, within the program's input slots), then the spare slots take the wallet's SMALLEST other UTXOs of the token
  (up to 3 token inputs in all: the 8 / 8 prototype's 8 would pass the standard mass). They join the order's ONE token change, their carriers come
  back as KAS change, so every placement consolidates the wallet (a single extra next to an exact cover is skipped: it would only re-create itself;
  a KRON change stays under its 10^9 output limit). An amend / cancel-replace top-up (`src/kob/cancel.ts`) keeps the fewest-inputs pick: its
  slots are shared with the old custody and the strays it sweeps.
* **Order views** need `state`, `current`, `custody.utxo`, `strays`, the position grouping needs `parent` / `repeat` (indexer available; without it My orders shows node-derived rows).
