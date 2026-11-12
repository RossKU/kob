# FAQ

Answers to questions people asked about KOB. Every claim links to the code or specification it rests on, pinned to the commit
named in each section (sections 1-4: [`633c06a1`](https://github.com/RossKU/kob/tree/633c06a182241ffcca40b3afff1fd161147e27b3)). Those links show
the files at that commit (`contracts/v2/` then); the live orders are in `contracts/orders/`.

**Miner Mode** is a testnet-10 proof of concept, not a released feature: a miner that also runs a KOB executor puts the fills it
builds into its own block templates and pays no network fee for them. It changes who pays the fee and how a transaction reaches a
block, nothing else below.

## 1. Custodial or non-custodial?

Non-custodial, in every mode. "Both cases" meant (a) the miner trading its own orders as a counterparty, or (b) the miner
including a fill between two other users. An executor's income is the crossing spread plus the tips the orders offer
([matcher spec §1](https://github.com/RossKU/kob/blob/633c06a182241ffcca40b3afff1fd161147e27b3/docs/spec/matcher.md#L15));
in Miner Mode it simply pays no network fee on the fills it includes in its own blocks.

## 2. Who signs what?

The same in every mode.

- **The maker signs once, when placing.** The funds move into an order covenant UTXO whose state fixes the price, amount,
  expiry and the maker's key, which receives every payout and refund
  ([`KobAsk.sil` L96](https://github.com/RossKU/kob/blob/633c06a182241ffcca40b3afff1fd161147e27b3/contracts/v2/KobAsk.sil#L96)).
- **A fill needs no maker signature.** The fill / settle entries take no signature; only `cancel` checks the maker's
  ([`KOBOrders.ag` L321-L352](https://github.com/RossKU/kob/blob/633c06a182241ffcca40b3afff1fd161147e27b3/contracts/argent/KOBOrders.ag#L321-L352)).
  The covenant itself checks that the maker receives at least their price.
- **The executor signs only its own inputs** (its funding for the fee, its tokens if it takes the other side).
- **Before a signature**, the KOB app decodes the transaction it built and checks it against what the user entered
  ([`decode.ts`](https://github.com/RossKU/kob/blob/633c06a182241ffcca40b3afff1fd161147e27b3/web/src/kob/decode.ts)), then shows one
  final confirmation: the order, the token, the KAS locked, the net KAS and the fee
  ([`confirm-model.ts`](https://github.com/RossKU/kob/blob/633c06a182241ffcca40b3afff1fd161147e27b3/web/src/ui/confirm/confirm-model.ts)).
  The wallet then shows its own approval.
- **Only SIGHASH_ALL.** KOB refuses any other signature hash type
  ([`tx.rs` L757-L766](https://github.com/RossKU/kob/blob/633c06a182241ffcca40b3afff1fd161147e27b3/crates/kob-protocol/src/tx.rs#L757-L766),
  [`decode.ts` L524](https://github.com/RossKU/kob/blob/633c06a182241ffcca40b3afff1fd161147e27b3/web/src/kob/decode.ts#L524)), and the
  covenant's own `cancel` requires it
  ([`KobAsk.sil` L309-L312](https://github.com/RossKU/kob/blob/633c06a182241ffcca40b3afff1fd161147e27b3/contracts/v2/KobAsk.sil#L309-L312)).

## 3. What stops front-running or self-preference?

There is no commit-reveal and no neutrality rule. The covenants bound what any executor or miner can take:

- **Taking orders wait in public first.** A market order, a limit order that crosses the book, and a Dutch order are
  placed with `activeFrom` = placement + 30 DAA (~3 s) by default
  ([`defaults.rs` L35-L36](https://github.com/RossKU/kob/blob/633c06a182241ffcca40b3afff1fd161147e27b3/crates/kob-protocol/src/defaults.rs#L35-L36),
  [`simple.ts` L286-L297](https://github.com/RossKU/kob/blob/633c06a182241ffcca40b3afff1fd161147e27b3/web/src/kob/orders/simple.ts#L286-L297)),
  and the covenant refuses any fill before it (`require(tx.daa >= activeFrom)`,
  [`KobAsk.sil` L237](https://github.com/RossKU/kob/blob/633c06a182241ffcca40b3afff1fd161147e27b3/contracts/v2/KobAsk.sil#L237)):
  the order sits publicly on chain before anyone can fill it, and its auction starts from there. A resting limit order is
  fillable at once: filling it early can only give the maker their own price.
- **Every limit is enforced on chain.** No fill is worse than the user's price.
- **Market and stop orders are on-chain decay auctions, and time is proven by lock time.** The price is a function of the
  auction time `t`, and the covenant requires `tx.daa >= t` (CLTV) and `t >= origin`
  ([`KobAsk.sil` L50-L56, L256-L257](https://github.com/RossKU/kob/blob/633c06a182241ffcca40b3afff1fd161147e27b3/contracts/v2/KobAsk.sil#L50-L57),
  [`KobBid.sil` L169-L170](https://github.com/RossKU/kob/blob/633c06a182241ffcca40b3afff1fd161147e27b3/contracts/v2/KobBid.sil#L169-L170)).
  Nobody can claim the cheaper end of an auction before that time has passed on chain; meanwhile the order sits publicly on
  chain and any other executor can fill it earlier at a better price. The worst case is the user's own bound: by default 3%
  over 20 s for a market order, 3% over 30 s for a stop
  ([order types](https://github.com/RossKU/kob/blob/633c06a182241ffcca40b3afff1fd161147e27b3/docs/spec/order-types.md#L205-L209)).
- **Stops need rested evidence.** A stop arms only on a resting order that has been exposed on chain at its price for at least
  `minRestDaa` (CLTV again), so a stop cannot be triggered by a quote placed and used at once
  ([`KobCondAsk.sil` L215-L255](https://github.com/RossKU/kob/blob/633c06a182241ffcca40b3afff1fd161147e27b3/contracts/v2/KobCondAsk.sil#L215-L255)).
- **Orders are public UTXOs and filling is permissionless**: any executor can compete for any order.

A miner can still order the transactions of its own block; it cannot break these bounds.

## 4. Settlement atomicity

A fill is one Kaspa transaction: every leg of it settles, or none does. Nobody signs at fill time, so "one side does not sign"
cannot strand funds. An unfilled order stays in its covenant. The maker can cancel it at any time with their signature, and
anyone can refund it to the maker (for a small keeper tip) once it expires, after 90 days without a fill, or when an IOC / FOK
order's life ends, whichever comes first
([`KobAsk.sil` L10](https://github.com/RossKU/kob/blob/633c06a182241ffcca40b3afff1fd161147e27b3/contracts/v2/KobAsk.sil#L10)).

Live checks on testnet-10: single-change attacks on every order covenant, all rejected by the node
([audit §8](https://github.com/RossKU/kob/blob/633c06a182241ffcca40b3afff1fd161147e27b3/docs/security/AUDIT-2026-10.md)).

## 5. How do market and limit orders work for a USD stablecoin when KAS keeps moving?

Pinned to commit [`7603c7b5`](https://github.com/RossKU/kob/tree/7603c7b5e4c08821a2556adb2bf771cca9897ee9).

Selling USDC for KAS happens on the USDC/KAS book, priced in KAS per whole USDC
([order types](https://github.com/RossKU/kob/blob/7603c7b5e4c08821a2556adb2bf771cca9897ee9/docs/spec/order-types.md#L22)). That price
moves with KAS/USD even though a USDC stays near $1. KOB has no oracle and no AMM: the price is the resting limit orders, and
arbitrageurs keep the book near the fair rate. KOB does not promise that market makers exist.

- **Market order.** An IOC decay auction: it starts at the best opposite price and moves toward your worst price, the slippage
  bound, by default 3% over about 20 s (`SLIPPAGE_BPS` = 300, `MARKET_AUCTION_DAA` = 200,
  [`defaults.rs` L31-L34](https://github.com/RossKU/kob/blob/7603c7b5e4c08821a2556adb2bf771cca9897ee9/crates/kob-protocol/src/defaults.rs#L31-L34);
  [order types](https://github.com/RossKU/kob/blob/7603c7b5e4c08821a2556adb2bf771cca9897ee9/docs/spec/order-types.md#L209)). It never
  fills beyond the bound, and what is not filled is returned.
- **Limit order.** A fixed KAS price: it does not follow KAS/USD. When KAS moves and the order becomes the cheap side, an arbitrageur
  takes it at your price. To keep it near $1, cancel and place it again at the new rate, or give it a short life: Good till a date
  (GTD) or Day, which ends at 00:00 UTC. Expiry is soft (a fill at the order's own limit stays possible until the refund lands), and a
  cancel is a maker-signed transaction that can lose a race against a fill
  ([order types](https://github.com/RossKU/kob/blob/7603c7b5e4c08821a2556adb2bf771cca9897ee9/docs/spec/order-types.md#L143-L148)).
- **The spread** is, roughly, how far KAS can move between two re-quotes of an order, plus fees. There is no fixed figure.
- **Token/token pairs.** Every order type also exists on a pair A/B, whose limit is in the quote token alone
  ([order types](https://github.com/RossKU/kob/blob/7603c7b5e4c08821a2556adb2bf771cca9897ee9/docs/spec/order-types.md#L137)). On an
  X/USDC book the price is in USDC, so a limit order keeps its dollar price whatever KAS does.

## 6. Where do token names and prices come from? Does KOB take prices from kascov?

Pinned to commit [`47792a0e`](https://github.com/RossKU/kob/tree/47792a0eba552e0469643c9d11b95d71b135c520).

- **Names and icons only come from kascov.io.** The market list takes a display ticker, a name and an image from kascov's public
  token list. Supply, decimals, prices and orders never do
  ([`kascov.ts` L1-L13](https://github.com/RossKU/kob/blob/47792a0eba552e0469643c9d11b95d71b135c520/web/src/data/kascov.ts#L1-L13)). Where kascov disagrees with what KOB derives, KOB wins and the list says so.
- **kascov's price and candles are an optional, third-party overlay. KOB uses them for nothing.** kascov replays the pools of other
  venues, so its price is a marginal pool price there, before fees; it is not a price from KOB's book. The
  chart has a small round kascov toggle, off by default; where kascov prices the token it draws kascov's price as one thin line behind KOB's candles, with a muted
  "kascov 0.01625" in the legend and a tooltip saying what it is. The market list adds one line to a price cell's tooltip, for example
  "kascov: 0.01625 KAS (-35%)" (against KOB's own price in that cell), and nothing visible changes. A token pair gets a figure only when
  kascov prices both tokens. The line never moves the chart's price scale, and no warning, ordering, check or figure of KOB reads it.
  **Privacy:** the list's tooltip uses the token list KOB already loads and asks for nothing more. The chart's candle request names the
  token you are viewing, so kascov.io sees it (and your IP address), but only while the toggle is on; with it off no request is made.
  Where kascov has no price for the token (or one of a pair's tokens, or its list is not available), the toggle is still there but faded and does nothing, with the reason as its
  tooltip, and nothing is asked of kascov. With kascov turned off in the deployment, there is no toggle and no line.
- **Prices come from KOB's own book on chain.** The KOB indexer reads the order UTXOs and the fills from the chain. The book is the
  resting orders, and the last price is the last fill that actually happened. It is not a number anyone types in. Makers choose their
  own limit prices, as on any exchange, but a price only becomes "the price" once someone trades at it.
- **USD figures are for display only:** the KAS price times KAS/USD from CoinGecko.
- The web app reads one indexer and does not cross-check it (the footer says so). Standalone mode builds the book from your own node
  instead.

## 7. I priced an ask too low and part of it filled. What happens when I fix it?

Pinned to commit [`47792a0e`](https://github.com/RossKU/kob/tree/47792a0eba552e0469643c9d11b95d71b135c520).

- **What filled is final.** A fill is settled on chain and cannot be reversed.
- **The rest stays on the book at the old price until your change confirms.** Cancel it, or use **Amend**, which re-prices the order in place with
  one maker-signed transaction ([payload spec](https://github.com/RossKU/kob/blob/47792a0eba552e0469643c9d11b95d71b135c520/docs/spec/kob1-payload.md#L31-L34)). Until that transaction
  confirms, a market buy can still take the rest at the old price: a cancel can lose a race against a fill (see section 5). Kaspa
  confirms in seconds, but a bot can be faster.
- **Once it confirms, the old price is gone.** The next market buy pays the best ask on the book then: your corrected price, or a
  cheaper ask from someone else.
- **There is no floor.** The last price stays on the chart as history and binds nobody; buyers pay the asks that are actually on the book.
- **The 24 h figures show the mistake as it happened.** The 24 h change compares the last trade with the price 24 h ago: the last trade
  at or before the start of the 24 h window, or the window's first trade when the token had no earlier trade
  ([`market.rs` L644-L654](https://github.com/RossKU/kob/blob/5729c692f73a8137b73c2dd0dad45cec0a17e78e/crates/kobd/src/indexer/market.rs#L644-L654), commit
  [`5729c692`](https://github.com/RossKU/kob/commit/5729c692f73a8137b73c2dd0dad45cec0a17e78e)). While the 0.0002 fill is the last trade, the change reads about -90% against a
  market that traded near 0.002 before the window; once later buys trade near 0.002 again, the change is back near 0%, the 24 h low reads 0.0002
  and the chart has a spike down. Only on a market with no trade before the window, where the 0.0002 fill is the window's first trade, does it become the reference:
  a later buy at 0.002 then shows +900% (10×, not +100%). The fill leaves the 24 h figures after 24 h and stays in the chart's history.
  When the window holds only one or two trades, the app shows the 24 h change muted, with "Based on 1 trade in 24 h" (or "2 trades") as its
  tooltip: such a figure compares two prints, not a trend.
  It is muted too, with "Thin book: under $X within 2% of the price" in the tooltip (a line of its own next to the trade count), when the smaller side of the book (the bids
  down to 2% below the mid, the asks up to 2% above it) is worth less than $X at the current KAS price. $X is 0.1% of the token's value (its fixed total supply times its
  price), at least $100 and at most $10,000, so a bigger token needs a deeper book; without a known supply or a KAS price there is no marker. The $100 to $10,000 range is
  CoinMarketCap's order sizes for its liquidity score, and the band is CoinGecko's +/-2% depth.
- **The ticket warns before you sign.** A sell priced 10% or more below the market, or a buy 10% or more above it, shows "check for a
  typing error" ([`guards.ts` L403-L420](https://github.com/RossKU/kob/blob/47792a0eba552e0469643c9d11b95d71b135c520/web/src/kob/guards.ts#L403-L420),
  [`common-issues.ts` L55](https://github.com/RossKU/kob/blob/47792a0eba552e0469643c9d11b95d71b135c520/web/src/kob/orders/common-issues.ts#L55)). "The market" is the mid of the best bid and the best ask,
  or the only side present ([`guards.ts` L32-L37](https://github.com/RossKU/kob/blob/47792a0eba552e0469643c9d11b95d71b135c520/web/src/kob/guards.ts#L32-L37)). On a book with no orders at all the last trade
  price is the reference instead (the sentence then says "the last trade price"), with the same 10% and 50% thresholds. With no orders and
  no trade there is nothing to compare with, so no warning shows.
