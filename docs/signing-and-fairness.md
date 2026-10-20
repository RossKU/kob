# Who signs, who pays, and what an executor cannot do

Answers to questions about KOB's settlement model and Miner Mode. Every claim links to the code or specification it rests on
(pinned to commit [`633c06a1`](https://github.com/RossKU/kob/tree/633c06a182241ffcca40b3afff1fd161147e27b3)).

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
