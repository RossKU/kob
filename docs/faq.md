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
- **The ticket warns before you sign.** A sell priced 10% or more below the market, or a buy 10% or more above it, shows "check for a
  typing error" ([`guards.ts` L403-L420](https://github.com/RossKU/kob/blob/47792a0eba552e0469643c9d11b95d71b135c520/web/src/kob/guards.ts#L403-L420),
  [`common-issues.ts` L55](https://github.com/RossKU/kob/blob/47792a0eba552e0469643c9d11b95d71b135c520/web/src/kob/orders/common-issues.ts#L55)). "The market" is the mid of the best bid and the best ask,
  or the only side present ([`guards.ts` L32-L37](https://github.com/RossKU/kob/blob/47792a0eba552e0469643c9d11b95d71b135c520/web/src/kob/guards.ts#L32-L37)). On a book with no orders at all there is
  nothing to compare with, so no warning shows yet.
