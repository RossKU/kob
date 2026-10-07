# KOB order types (protocol v3)

Public reference: every order type KOB offers, the covenant that enforces it, its entries, and what
the user can expect. Contracts: `contracts/v2/*.sil`. Matching, keeper and wallet rules:
`matcher.md`.

Every order is a covenant UTXO on Kaspa. Its price, quantity, custody, trigger and time rules are
checked on chain; a matcher only proposes transactions. Anyone may match (a miner included) and
earns the crossing spread plus the tips. Every order type exists on a token's KAS book (X/KAS) and on a token/token pair
(A/B, *Pair orders*).

## Amounts, prices and rounding

This section is normative for every covenant and for all off-chain code (protocol library, builders, indexer, planner,
SDK, wallet): off-chain code computes exactly what the covenants compute. The covenant source of the rule is the
`KobAsk` header (`contracts/v2/KobAsk.sil`); every other template refers to it.

**One rule.** An order is an `amount` of its **base token**, in base units (any positive amount; there are no lots), at
a limit `price` in its **quote token** per WHOLE base token: `price` quote units per `scale` base units, where
`scale = 10^decimals` of the base token.

- Orders of an X/KAS book (asks, bids, conditional and if-done orders): base X, quote KAS; `price` is sompi per whole X.
- Pair orders A/B (`KobPair`, `KobCondPair`, `KobIfdPair`): base A, quote B; `price` is base units of B per whole A, and
  the denominator is the scale of A. A pair order's state carries the scales of both tokens (`sScale` / `tScale`, or
  `aScale` / `bScale`).
- Tips are always KAS: `tip` is sompi per whole base token (a pair order: per whole A). In a KAS-quoted order the tip is
  part of the all-in price; in a pair order it is a separate KAS amount, `⌊n·tip / scale(A)⌋` per fill, prefunded on the
  order UTXO.
- `scale` is a per-order state field. Builders allow `1 ≤ scale ≤ 10^9` and require a power of ten; the wallet sets
  `10^decimals` of the base token, capped at `10^9`. Prices of two orders are comparable only at the same `scale`
  (books and trigger evidence are per `scale`, `matcher.md` §2, §4; a pair book per scale of A and scale of B).

**Quote value.** The quote value of n base units at a rate r (quote units per whole base token) is `n·r / scale`, rounded
in the MAKER's favour: **up** (ceil) for what a maker receives, and for the budget a bid consumes from its escrow;
**down** (floor) for what a maker pays.

| Kind | Rounded up (the maker receives; escrow consumed) | Rounded down (the maker pays) |
|---|---|---|
| `KobAsk` | proceeds `quoteOf(n, p(t) − tip)` | — |
| `KobBid` | budget used `quoteOf(n, pMax + tip)` (`pMax` = `price`, or `priceEnd` of a rising bid when higher) | spend `quoteOf(n, p(t) + tip)` |
| `KobCondAsk` | proceeds `quoteOf(n, legPrice − tip)`; repeat: the budget returned to the entry `quoteOf(n, rptPrice)` | — |
| `KobCondBid` | repeat: the proceeds `quoteOf(n, rptPrice)` and prefund `quoteOf(n, rptPre)` it releases | spend `quoteOf(n, legPrice + tip)` |
| `KobIfdBid` | merge: the budget back `quoteOf(m, price + tip)` | spend `quoteOf(n, p + tip)` |
| `KobIfdAsk` | proceeds `quoteOf(n, p − tip)`, the exit's prefund `quoteOf(n, prefund)`; merge: `quoteOf(m, prefund)` back, and on the exit's sell-out the maker's proceeds `quoteOf(m, price − tip)` | — |
| `KobPair` | ask: B received `quoteOf(n, p(t))` | bid: B paid, EXACTLY `quoteOf(n, p(t))`; both sides: the KAS tip `quoteOf(n, tip)` |
| `KobCondPair` | ask: B received `quoteOf(n, legPrice)`; repeat: the entry's budget `quoteOf(n, rptPrice)`, a sell-first exit's proceeds `quoteOf(n, rptPrice)` and prefund back `quoteOf(n, rptPre)` | bid: B paid, at most `quoteOf(n, legPrice)`; the KAS tip |
| `KobIfdPair` | sell-first: the proceeds `quoteOf(n, p)` and the exit's prefund `quoteOf(n, prefund)`; merge: the budget back `quoteOf(m, price)` (buy-first), the prefund back `quoteOf(m, prefund)` (sell-first) | buy-first: B paid, at most `quoteOf(n, p)`; the KAS tip |

The KRON twins (`…Kron`) compute the same values; the pair templates serve both families.

**Exact split multiplication.** `quoteOf` is computed without 128-bit arithmetic and without an intermediate overflow
(integer division, `c = 0` rounds down, `c = scale − 1` rounds up):

```
q = n / scale;  m = n mod scale
quoteOf(n, r, c) = q·r + m·(r / scale) + (m·(r mod scale) + c) / scale
```

The pair templates write it with an explicit denominator, `quoteOf(n, r, d, c)` with `d` = the scale of A (and, for the
trigger comparisons, the scale of B: *Pair trigger rule*).

With `n = q·scale + m` and `r = Q·scale + R`, `n·r / scale = q·r + m·Q + m·R / scale`; the first two terms are integers, so
the rounding applies only to the last one, and the result is exactly `⌊n·r / scale⌋` (c = 0) or `⌈n·r / scale⌉`
(c = scale − 1) for `n, r ≥ 0`. The intermediates are bounded: `q·r` and `m·(r / scale)` never exceed the result, and
`m·(r mod scale) + c ≤ (scale − 1)·scale`. The formula is therefore exact whenever `scale·(scale − 1) < 2^63` (the contract
headers state the simpler `scale² < 2^63`, `scale ≤ 3,037,000,499`) and the result itself fits in a signed 64-bit
integer. Off-chain code mirrors it (in 128-bit arithmetic or with the same split) and its tests assert equality with the
covenants.

**Overflow.** The engine checks every product and sum: an overflow FAILS the script, it never wraps. An out-of-range value
can therefore only make a fill impossible (fail closed), never pay a maker less. To keep every order live, builders
refuse, and indexers do not list, an order unless every `scale` it carries is a power of ten with `scale ≤ 10^9` and,
computed in 128-bit arithmetic, `amount·r / scale < 2^62` for every rate r the order carries (`price`, `price + tip`,
`pMax + tip`, `priceEnd`, `tpPrice`, `stopPrice`, `rptPrice`, `prefund`; a pair order also `tip`, its worst leg, a pair
entry's `entryStop`, `price + prefund` and its exit's legs). A stop leg has its own bound (`stopPrice ≤
922,337,203,685,477`, *Common semantics*). A KRON token UTXO holds at most 10^9 base units (both pinned KRON programs
refuse more): a pair order whose KRON amount or custody exceeds that is refused.

**Why rounding cannot be farmed (proof sketch).** Ceil is superadditive and floor subadditive:

```
⌈x⌉ + ⌈y⌉ ≥ ⌈x + y⌉ ≥ x + y          ⌊x⌋ + ⌊y⌋ ≤ ⌊x + y⌋ ≤ x + y
```

1. Along any sequence of fills `n₁ … n_k` of an order (in any number of transactions, `N = Σ nᵢ`), every fill pays a
   seller at least the exact value of its amount at the limit and charges a buyer at most that value, so the sum over the
   fills is at least (seller) or at most (buyer) the exact value of the total: `Σ ⌈nᵢ·r / s⌉ ≥ ⌈N·r / s⌉ ≥ N·r / s` and
   `Σ ⌊nᵢ·r / s⌋ ≤ ⌊N·r / s⌋ ≤ N·r / s`. (A rate that moves with time, an auction, is fixed within one fill; the
   argument holds fill by fill at each fill's rate.)
2. Splitting a fill can only move value TO the maker, less than one quote unit per fill, and costs the splitter one more
   transaction fee (and is bounded by the minimum fill, below). Rounding cannot be farmed.
3. A bid's escrow is consumed at the ceil budget `used(n)`, and `Σ used(nᵢ) ≥ ⌈N·(pMax + tip) / s⌉`, so split fills never
   buy more than the escrow funds. The difference `used(n) − spend(n)` (the rounding and a rising bid's headroom) rides
   on the maker's delivery. A pair bid's escrow is a token custody: each fill releases its floor, and the floors of the
   fills never exceed the floor of the total (subadditivity), so an escrow of `⌊N·pMax / scale(A)⌋` pays every split;
   the wallet adds one base unit per possible fill so that a partial fill always leaves a positive rest (*Pair orders*).
4. Where one release is split between several maker-owned outputs (a repeat merge: the exit's payout to the maker and
   the budget or prefund returned to the entry), both covenants compute the SAME rounded amounts (the exit releases
   `quoteOf(m, rptPrice)` and `quoteOf(m, rptPre)` rounded up, and the entry requires exactly those values, since
   `rptPrice` and `rptPre` are its own rates), so no remainder is left between the two for the filler.

**Minimum fill (`minFill`).** Every kind carries `minFill`, in base units of its base token:

- asks, conditional orders, if-done entries and every pair order (both sides): a fill takes `n ≥ minFill` unless it takes
  everything left (`n = amountLeft`);
- `KobBid` (its quantity is its escrow): `n ≥ minFill` unless the bid terminates because less than one minimum fill of
  buying power is left (`left − deliveryCarrier − reserve < quoteOf(minFill, pMax + tip)` rounded up); a `KobBid`
  requires `minFill > 0`.

Why: every fill of a bid-side order (`KobBid`, `KobCondBid`, `KobIfdBid`, the exits of `KobIfdAsk`, every pair order's
delivery) moves a `deliveryCarrier` (2 KAS by default) from the maker's funds onto a new token UTXO, every fill of an
if-done entry also creates an exit with its own carrier, and every fill restarts a TWAP / DCA interval. Without a minimum
a griefer could split an order into 1-base-unit fills, turning the maker's funds into carriers on dust token UTXOs,
fragmenting the maker's tokens and stretching a TWAP. For a non-repeating order `minFill` bounds the number of fills
(deliveries, carriers, exits, token UTXOs, payouts) to `⌈amount / minFill⌉`, exactly as the if-done `minLots` of protocol
v2.6 did. A REPEATING if-done entry has no such bound over its life: every merge re-arms an amount, and the "takes
everything left" exception applies again after each one, so an entry fill (and so an exit) can be as small as what was
re-armed, down to the exit's own `minFill`; the bound holds per cycle, not per entry. A `KobBid`'s quantity is its escrow:
its FOK completeness and the end of a GTC bid are exact in value terms (it ends when less than one minimum fill of buying
power is left: the unfilled remainder, worth less than one minimum fill plus one sompi, returns to the maker).

Wallet default: the amount worth 10 KAS (`DEFAULT_MIN_FILL_SOMPI`, a notional independent of the carrier) at the order's
limit price, at least 1 base unit and at most the order's amount (a pair order: the amount of A worth 10 KAS on A's KAS
book); IOC, FOK and market orders 1 base unit; if-done entries `⌈amount / 4⌉` (*Defaults*). At the default carrier the
carriers a filler can make the maker lock are at most 20% of the order's notional, and one fill's fee (paid by the filler)
is at most 0.6% of 10 KAS on every token program and 0.3% to 1.4% for a pair fill (KaspaCom on both sides the highest;
`DEFAULT_MIN_FILL_SOMPI`).

## Common semantics

- **All-in limits.** A KAS-quoted limit includes every fee: a sell of n base units receives at least
  `⌈n·(price − tip) / scale⌉`, a buy of n pays at most `⌊n·(price + tip) / scale⌋` (*Amounts, prices and rounding*). A
  pair order's limit is in the quote token alone (a sell of n of A receives at least `⌈n·price / scale(A)⌉` of B, a buy
  pays at most `⌊n·price / scale(A)⌋`), and its tip is KAS on top. `tip` is an optional priority tip (wallet default 0).
  Whatever a crossing leaves between two limits goes to the matcher.
- **Partial fills** are allowed everywhere except FOK; the remainder of a resting order keeps resting unchanged (an
  IOC remainder is returned). Every fill but the last takes at least the order's `minFill`.
- **Amounts.** Quantities are any number of base units of the token; prices are per whole token (`scale` base units).
- **Expiry is soft.** An order is refundable by anyone (keeper, for `refundTip`) from its expiry;
  until the refund lands a fill at the order's own limit is still possible. GTC = refundable after
  90 days without activity. Day orders end at **00:00 UTC** (`matcher.md` §10.10).
- **Cancel** is maker-signed (SIGHASH_ALL) and may lose a race against a fill.
- **Triggers** (stops, trailing, stop entries) are touches. A KAS-quoted stop arms, or trails, only in a transaction that
  fills a plain resting order of the same token and `scale` at or beyond the stop (for a trail, far enough beyond it for
  at least one step) that had rested there for at least `minRestDaa` (default 5 s) and trades at least the order's
  threshold `minTouch` (base units, chosen per order). A pair stop reads the same kind of evidence in one of two modes:
  two plain resting KAS-book orders, one of A and one of B, filled together (their implied rate), or a resting `KobPair`
  of the same pair (*Pair trigger rule*). The covenant reads that fill itself; a matcher's word is never a trigger, and
  nothing persists after the transaction. Matchers arm stops inside the batches that make such fills; a qualifying fill
  that no batch uses leaves the stop waiting for the next one (`matcher.md` §4). Conditional and if-done fills never
  trigger, and pair fills never trigger a KAS-quoted order.
- **Auctions.** Market orders (on a token and on a pair), triggered stops and stop entries fill as short
  auctions from the market toward their worst price, so the matcher keeps only a competitive margin.
- **Custody.** Tokens a sell order sells sit in exactly one token UTXO owned by the order, holding exactly `amountLeft`
  (a pair bid's B escrow, a sell-first pair entry's B prefund: exactly the state's `custody`); tokens of the order's own
  token(s) sent to it from outside (a pair order: of A or of B) are inert and only the maker's cancel moves them. Tokens
  of any OTHER token (another covenant id) sent to an order are NOT protected: every spend of the order (fill, refund,
  update, cancel) authorises them, so whoever builds that transaction may move them. Never send tokens to an order
  (`matcher.md` §1.2).
- **Positional outputs.** Everything an order owes is paid, delivered or returned at a position no
  other order can claim: its own input index, or (the unsold rest of an IOC sell, a custody's rest) the
  index of its own custody input. One output never serves two orders.
- **Stop bands** are exact: `stop ∓ ⌊stop × bps / 10000⌋` quote units per whole token, rounded in the maker's
  favour, at any stop price (a stop leg needs a stop of at most 922,337,203,685,477 quote units per whole token).
- **No on-chain price-time priority**; the tip is the priority lever. Self-trade prevention is the
  wallet's job. Token↔token trades are pair orders (*Pair orders*): each enforces only its own guarantees, and a matcher
  fills it through the two KAS books, against opposite pair orders of the same pair, or from its own tokens
  (`matcher.md` §3.5).

## Covenants and entries

| Covenant | Holds | Entries (who) |
|---|---|---|
| `KobAsk` | tokens (custody) + carrier | `settle(n > 0)` fill (anyone), `settle(0)` refund (anyone after expiry), `cancel` (maker) |
| `KobBid` | KAS budget | `fill(n)` (anyone), `refund` (anyone after expiry), `cancel` (maker) |
| `KobCondAsk` | tokens (custody) + carrier | `settle(n, leg)` fill on the take-profit leg or the stop leg (the stop leg arms in the same fill, or is already armed) (anyone), `settle(0)` refund, `update` arm / trail (anyone, next to the evidence fill and never next to its repeat entry; the keeper takes up to `keeperTip`), `cancel` (maker) |
| `KobCondBid` | KAS budget | `settle(n, leg)` (anyone), `update` arm / trail (anyone, next to the evidence fill and never next to its repeat entry), `refund`, `cancel` |
| `KobIfdBid` | KAS budget + carriers | `fill(n > 0)` entry fill creating one exit (anyone), `fill(merge)` re-arm a repeat (anyone, with its exit's take-profit), `update` arm a stop entry (anyone, next to the evidence fill; needs `amountLeft > 0` and `entryStop ≤ price`), `refund`, `cancel` |
| `KobIfdAsk` | tokens (custody) + prefund + carriers | `settle(n > 0)` entry fill creating one exit, `settle(0)` refund, `settle(merge)` re-arm a repeat, `close` refund with nothing left, `update` (as `KobIfdBid`; needs `amountLeft > 0` and `entryStop ≥ price`), `cancel` |
| `KobPair` (pair A/B, either side, tokens of either family) | the token it sells (custody of S: A for an ask, the B escrow for a bid) + KAS for its delivery carriers and tip | `settle(n > 0)` fill (anyone), `settle(0)` refund (anyone after expiry, 90 days idle or the IOC / FOK kill), `cancel` (maker) |
| `KobCondPair` (pair A/B, either side) | custody of S + KAS for its carriers and tip | `settle(n > 0, upd 0)` fill on the take-profit / limit leg or the stop leg (armed, or armed in this fill by the evidence) (anyone), `settle(0, upd 0)` refund, `settle(0, upd 1)` update: arm (`k = 0`) or trail (`k ≥ 1`) next to the evidence (anyone; never next to its repeat entry; the keeper takes up to `keeperTip`), `cancel` (maker) |
| `KobIfdPair` (pair A/B, either side) | buy-first: the B escrow; sell-first: A (exactly `amountLeft`) and a B prefund; + carriers | `fill(n > 0)` entry fill creating one `KobCondPair` exit (anyone), `fill(merge)` re-arm a repeat, `fill(0, upd 0)` refund (also of an empty repeating entry), `fill(0, upd 1)` update: arm a stop entry (needs `amountLeft > 0` and the stop on the right side of the limit), `cancel` (maker) |

The KRON family has a twin of every KAS-quoted kind (`KobAskKron` … `KobIfdAskKron`, `contracts/adapters/kron/README.md`);
each pair template is one template for both families of each of its tokens (its state names the family of A and of B).

## Order types

Every order type below exists on a pair too (*Pair orders*); the table names the KAS-quoted covenants.

| Order type | Side | Covenant and key fields | Execution | What the user sees |
|---|---|---|---|---|
| Limit GTC | sell / buy | `KobAsk` / `KobBid`, tif 0 | rests; any crossing fills at the limit or better | fills at the limit or better; lives until cancelled or 90 days idle |
| GTD / date | both | any, `expiryDaa` | as limit until expiry | refundable from the date |
| Day | both | any, `expiryDaa` + placement `deadline` | as limit until 00:00 UTC | ends at 00:00 UTC (09:00 JST) |
| Timed activation | both | any, `activeFrom` | no fill before `activeFrom` | starts at the chosen time |
| Market | both | `KobAsk` / `KobBid`, tif 1 (IOC), `slope`, `decayStep` 1, `minFill` 1 | IOC auction from the touch to the slippage bound (default 3% over 20 s) | fills near the market, never beyond the bound; unfilled rest returned |
| IOC / FAK | both | tif 1 | fills what crosses now; remainder returned in the same transaction (sell: at the custody input's index) | killed by anyone after 60 s at the latest |
| FOK | both | tif 2 | all or nothing in one transaction | killed by anyone after 60 s at the latest |
| Streaming (quote-and-execute) | both | as market, from the displayed price | IOC or FOK auction | fills at the displayed price or better within the tolerance |
| Stop-market | sell / buy | `KobCondAsk` / `KobCondBid` stop leg, `slipBps` 300 | armed by a fill in the same transaction of a resting order quoting at or beyond the stop (sell stop: an ask ≤ stop; buy stop: a bid ≥ stop); 30 s auction from the stop to stop ∓ 3% | triggers on a traded resting quote at or beyond the stop (≥ 5 s exposure, ≥ the threshold); worst case 3% |
| Stop-limit | sell / buy | same, band = the limit | as stop-market, never beyond the limit | may not fill if the market jumps past the limit |
| Trailing stop | sell / buy | `trailStep`, `trailGap`, `trailWait` | matchers move the stop by every step a fill of the opposite side justifies (sell stop: a resting bid; buy stop: a resting ask), at most once per `trailWait` | the stop follows the market in steps |
| Take-profit | sell / buy | `KobCondAsk` / `KobCondBid` limit leg (or a plain limit) | as limit | as limit |
| OCO | sell / buy | one `KobCondAsk` / `KobCondBid`, both legs | whichever leg fills first; both legs share one UTXO | one leg cancels the other by construction; partial fills keep both legs |
| IFD (buy-first / sell-first) | both | `KobIfdBid` → `KobCondAsk` exits; `KobIfdAsk` → `KobCondBid` exits | every entry fill of n base units creates one exit for exactly n; `minFill` bounds the number of fills of a non-repeating entry (a repeating entry: per cycle) | one position: entry plus exits; exits default to GTC |
| IFO / bracket | both | as IFD with TP + SL exit legs | each exit is an OCO for its amount | as OCO per filled part |
| IFD / IFO with stop entry | both | `entryStop`, `bandDaa` | armed by a fill of a resting order at or beyond the trigger (buy-stop: a bid ≥ `entryStop`; sell-stop: an ask ≤ `entryStop`), then an auction from `entryStop` to the limit `price` over `bandDaa` (the covenant requires `entryStop ≤ price` for the buy entry, `entryStop ≥ price` for the sell entry; *Stop entries* below) | entry triggers like a stop order |
| **Repeat IFD / IFO** | both | IFD entry with `rptAmount = 1 + K·N`; exits carry `parent`, `rptPrice`, (`rptPre`), `rptUntil` | every take-profit of m base units re-arms m base units of the ORIGINAL entry (same price, same exit) in the same transaction; the maker receives the profit, the entry gets the budget (or tokens and prefund) back | repeats up to K times per base unit and at most 90 days; partial fills at every step; a stop-loss ends the repeat for its amount; cancel = entry + exits in one transaction |
| TWAP | sell | `KobAsk` `interval`, `maxFill` (+ `slope`) | at most `maxFill` base units per `interval`; each slice may auction | rate-limited sell |
| DCA | buy | `KobBid` `interval`, `maxFill` (+ `slope`) | as TWAP | rate-limited buy |
| Dutch / decay, rising bid | sell / buy | `slope`, `priceEnd`, `decayStep` | price moves step by step toward `priceEnd` | fills at the first profitable moment |
| Cancel | both | `cancel` | maker, SIGHASH_ALL | may lose to a fill |
| Cancel-replace / amend | both | `cancel` + new placement, one transaction; a plain ask whose `scale` and `amountLeft` stay, or a plain bid whose token and `scale` stay, is amended IN PLACE (`cancel` continuing its covenant id, an `AMEND` record); the wallet re-plans every other kind from the ticket (trailing stops, IFD / IFO entries, every pair order) as one cancel-replace | atomic | an amended armed stop restarts unarmed; an in-place amend keeps the order id and its custody (no token transfer, a fraction of the bytes) |
| Close | both | market sell / buy of the held amount | as market | spot: closing = selling the held tokens |

## Repeat IFD in detail

A repeating entry books each exit it creates while re-arms are left (`rptAmount − 1` base units of re-arms;
the wallet sets `rptAmount = 1 + K·N` for K repeats of N base units). When a booked exit takes profit on m base units,
the same transaction must spend the entry in merge mode, so the entry is back with m more base units at its
original price, trigger and exit parameters; nothing about the order can be changed by the
matcher. Per cycle and whole token (each amount rounded in the maker's favour, *Amounts, prices and rounding*):

| | Buy-first (buy low, sell high) | Sell-first (sell high, buy back low) |
|---|---|---|
| entry fill | buys at ≤ `price` + tip, exit gets the tokens | sells at ≥ `price` − tip, exit gets proceeds + `prefund` |
| take-profit | sells at ≥ `tp` − tip | buys back at ≤ `tp` + tip |
| maker receives | (tp − tip) − (price + tip), at the take-profit | (price − tip) − (tp + tip), at the take-profit |
| entry gets back | the budget `price + tip` (`rptPrice`); carriers when the exit sells out | the tokens and `prefund` (`rptPre`); everything else the exit held when it sells out |

The position never holds more than its N base units (entry amount + exit amounts), every cycle's proceeds and
amounts are accounted exactly, and each cycle must complete within 90 days (`rptUntil`); after that,
or once the entry has expired, a booked exit may take profit without re-arming. A stop-loss exit never
re-arms. A booked exit's amount is below 2^53 (the merge argument `−(k·2^53 + m)`).

The entry re-arms only from an exit that is exactly one it books: same template, same committed
terms (maker, legs, trigger and band rules, carriers), `parent` = the entry and the entry's own
`rptPrice` (and `rptPre`), and only from that exit's own take-profit fill (each side checks the other's leading 8-byte
push: the exit's `n = m`, the entry's merge argument). An exit's `update` (arm / trail) never runs in a transaction that
spends its entry. A covenant id does not show which transaction created it, so a look-alike exit that someone else
builds and pays for cannot be refused; it can only give: it pays this maker on this maker's terms, and the merge
returns the full budget (buy-first) or at least the prefund of the re-armed amount (sell-first), so a merge never
costs the entry.

The pair entries (`KobIfdPair`) repeat the same way in the quote token B (*Pair if-done entries*).

## Stop entries (IFD / IFO)

A stop entry (`entryStop > 0`) cannot fill until it is armed by a touch (*Common semantics*): either in the
entry's own fill, or by `update` next to the evidence fill (the keeper takes up to `keeperTip` from the
entry's UTXO). `update` requires `amountLeft > 0` and the stop on the right side of the limit, the same ordering a
fill requires (`entryStop ≤ price` for the buy entry, `entryStop ≥ price` for the sell entry). Once armed the entry
fills as an auction from `entryStop` to `price` over `bandDaa` DAA (`bandDaa` 0: at `price` at once).

`armed` is 0 (unarmed), 1 (armed; the auction origin is the DAA score of the entry UTXO) or the auction origin
itself. A fill that leaves an amount carries the origin into the continuation, so partial fills do not restart the
auction. A merge (*Repeat IFD in detail*) treats it the same way: into an entry that is armed but not yet filled
(`armed` 1) with `bandDaa > 0` it records the origin (`armed` := the entry UTXO's DAA), so re-arming does
not restart the auction; into an entry with nothing left it resets `armed` to 0, and a new trigger is needed. The pair
entries follow the same rules.

## Pair orders

A pair A/B is two tokens (covenant id, token program, scale), each of either family (KCC-20 or KRON); A is the base,
B the quote, and A ≠ B (the wallet, the builders and the indexer refuse A = B). Every order type of the KAS ticket
exists on a pair, with the same amount rule: an amount of A in base units at a price in base units of B per whole A,
rounded in the maker's favour, with the same `minFill` rule. Three templates cover them, each ONE template for both
sides and all four family combinations (`contracts/v2/KobPair.sil`, `KobCondPair.sil`, `KobIfdPair.sil`; the headers
are the reference):

| Order type | Pair template | Key fields |
|---|---|---|
| Limit GTC / GTD / day / timed activation, IOC / FAK, FOK | `KobPair`, side 1 (ASK: sells A) or 2 (BID: buys A) | `price`, `tif`, `expiryDaa`, `activeFrom`, `minFill`, `tip` (KAS) |
| Market, streaming, close | `KobPair` IOC (or FOK) auction: ASK decaying, BID rising | `slope`, `priceEnd`, `decayStep` 1, `minFill` 1 |
| Dutch / decay, rising bid | `KobPair` | `slope`, `priceEnd`, `decayStep` |
| TWAP (sell A) / DCA (buy A) | `KobPair` | `interval`, `maxFill` (+ `slope`) |
| Stop-market, stop-limit, trailing stop, take-profit, OCO | `KobCondPair`, side ASK (sell stop below the market, take-profit above) or BID (buy stop above, limit below) | `stopPrice`, `slipBps`, `bandDaa`, `trailStep`, `trailGap`, `trailWait`, `tpPrice`, `minTouch`, `minRestDaa`, `keeperTip` |
| IFD / IFO / bracket, stop entry, repeat | `KobIfdPair`, side BID (buy-first, exit `KobCondPair` ASK) or ASK (sell-first, exit `KobCondPair` BID) | `price`, `prefund`, `entryStop`, `bandDaa`, `exitState`, `rptAmount`, `deliveryCarrier`, `exitCarrier` |

`KobPair` and `KobCondPair` name their tokens by role: S, the token the order sells and holds (A for an ask, B for a
bid), and T, the token it buys and receives (B for an ask, A for a bid), each with its covenant id, program hash,
prefix and suffix lengths, family and scale, and the extension commitment of T's deliveries. `KobIfdPair` names A and B
(both with an extension commitment for the new outputs of that token). The record family of a pair order is the family
of A (`kob1-payload.md`).

**Option (2): each order enforces only its own guarantees.** A pair order checks what it receives and pays (at least /
at most its price in B), the exact amounts of the tokens it holds, the stray guards of BOTH tokens (no input of either
token owned by its covenant id other than its own custodies) and its positional outputs. It does not check where the
counterparty side comes from: a matcher may route it through the KAS books of A and B, net it against any number of
opposite pair orders, or fill it from its own tokens (`matcher.md` §3.5). Prices are recorded only from KAS-book order
fills: a pair fill against a pair order or inventory is volume only (`matcher.md` §3.5). *Decision 2026-10-05: this
reverses the earlier rule that a token/token order may settle only through the two KAS books or against one opposite
order.*

### Plain pair orders (`KobPair`)

A fill of n base units of A (`n ≥ minFill` unless `n = amountLeft`, `n ≤ maxFill` for TWAP / DCA, the order UTXO at least
`interval` DAA old) at the quote `p(t)`:

- **ASK** (sells A for B): releases exactly n of A from its custody and receives `tOut ≥ ⌈n·p(t) / scale(A)⌉` of B at
  output i (its input index), owned by the maker (KCC-20 scheme 0 with the order's `tExt`, KRON `id_type` 3). The custody
  holds exactly `amountLeft` (`custody = amountLeft`, checked on every fill): every displayed amount can be taken.
- **Custody identity.** The custody of S is a token UTXO owned by the order's covenant id holding exactly `custody`, of
  the KCC-20 extension commitment the state names (`sExt`; zero for a KRON S): units of S's covenant id with another
  commitment are another token, and every entry that reads the custody (fill, refund, IOC / FOK end) refuses them.
  `KobCondPair` pins its custody the same way (`sExt`; an if-done exit carries its entry's `aExt` / `bExt`), and
  `KobIfdPair` pins its A custody, its B escrow or prefund and the custody of a merging exit to `aExt` / `bExt`.
- **BID** (buys A with B): receives exactly n of A at output i and releases EXACTLY `⌊n·p(t) / scale(A)⌋` of B from its
  escrow custody (`custody`, exact). A bid shows only quotes its escrow funds. The builders refuse an escrow below
  `⌊amountLeft·pMax / scale(A)⌋` (`pMax` = `price`, or `priceEnd` of a rising bid); the wallet funds
  `⌈amountLeft·pMax / scale(A)⌉` plus one base unit per possible fill (`PairState::bid_escrow`), so that a partial fill
  always leaves a positive rest; the escrow left after the last fill returns to the maker.
- **Decay.** ASK `p(t) = max(priceEnd, price − slope·⌊(t − origin) / decayStep⌋)`, BID `p(t) = min(priceEnd, price +
  slope·⌊…⌋)`; `origin` = `activeFrom`, or for a TWAP / DCA slice `max(activeFrom, UTXO DAA + interval)`; `t` proven by
  CLTV (understating it only moves the price toward the maker).
- **KAS.** The order UTXO prefunds `deliveryCarrier` per fill (the KAS on the maker's T output) and the tip of the whole
  amount: a fill releases at most `⌊n·tip / scale(A)⌋` sompi. A partial fill requires the order UTXO to hold at least
  `deliveryCarrier + tip(n)`, puts at least `deliveryCarrier` on the delivery, keeps the rest on the continuation and
  the custody's carrier on the custody rest; a last, IOC or FOK fill returns the custody rest (if any) to the maker at the
  custody's index with its carrier and everything but the tip at output i. The order funds its own carrier and tip: a
  filler never pays to take it.
- **Rest.** A partial GTC fill continues the same script with `amountLeft − n` and `custody − sOut` (the custody rest at
  the custody input's index, owned by the order); an IOC remainder returns at once; FOK fills everything or nothing.
- **Refund.** Anyone, from `min(expiryDaa, UTXO DAA + 90 days, IOC / FOK kill = max(UTXO DAA, activeFrom) + 600)`: the
  whole custody back to the maker at output i with every carrier, minus `refundTip`; a refund carries no input of T.

**Takeable quotes.** Every quote a `KobPair` shows can be taken by anyone at that quote: an ask's custody is its whole
remaining amount, a bid pays exactly its quote from its escrow, and the order funds its own delivery carrier and tip.
So a fill of a resting `KobPair` is a real trade at its price, and it is trigger evidence of the pair conditionals of the
same pair (*Pair trigger rule*, evidence mode 1).

### Pair conditionals (`KobCondPair`)

The logic of `KobCondAsk` (side ASK) / `KobCondBid` (side BID) with the price in B per whole A and the settlement of
`KobPair`: one UTXO, both legs, partial fills keep both.

- **ASK** (sells A; its custody is exactly `amountLeft` of A): stop leg BELOW the market (sell stop), take-profit leg above
  it. A fill of n pays the maker `tOut ≥ ⌈n·legPrice / scale(A)⌉` of B at output i.
- **BID** (buys A; its custody is a B escrow): stop leg ABOVE the market (buy stop), limit leg below it. A fill of n
  delivers exactly n of A at output i and releases at most `⌊n·legPrice / scale(A)⌋` of B. The builders refuse an escrow
  below `⌊amountLeft·worst / scale(A)⌋` (`worst` = the highest leg price, the stop band included); the wallet adds one
  base unit per possible fill (`CondPairState::bid_escrow`).
- **Legs.** Take-profit / limit leg (0): `tpPrice`, at any time. Stop leg (1): only once armed; `legPrice = stop −
  ⌊stop·bps / 10⁴⌋` (ASK) or `stop + ⌊stop·bps / 10⁴⌋` (BID), `bps = slipBps` (`bandDaa` 0) or the auction
  `slipBps·min(t − origin, bandDaa) / bandDaa`; the fill that arms the stop with `bandDaa > 0` trades at the stop itself.
  `armed` 0 / 1 / origin as in the KAS kinds. A conditional pair order has no `tif` (GTC / GTD / day).
- **Update** (`settle` with `n = 0`, `upd = 1`): anyone, next to the evidence; `k = 0` arms an unarmed stop, `k ≥ 1`
  trails it by k steps (*Pair trigger rule*); the keeper takes up to `keeperTip` from the order UTXO; it spends no token
  input owned by the order and never runs next to its repeat entry.
- **KAS, refund.** As `KobPair` (the order UTXO prefunds the delivery carrier per fill and the tip; refund from
  `min(expiryDaa, UTXO DAA + 90 days)`: the whole custody back to the maker at output i with every carrier, minus
  `refundTip`).

### Pair if-done entries (`KobIfdPair`)

The logic of `KobIfdBid` (side BID) / `KobIfdAsk` (side ASK) with prices in B per whole A. Every fill of n base units of A
(`n ≥ minFill` unless it takes everything left; wallet default `minFill = ⌈amount / 4⌉`) creates its own exit, a FRESH
`KobCondPair` covenant whose state is the committed `exitState` (the `KobCondPair` state up to `armed`, 432 bytes) with
`amountLeft := n`, `custody :=` the exit's custody, then the repeat fields the entry writes (`parent`, `rptPrice`,
`rptPre`, `rptUntil`). The entry recomputes the exit's consensus genesis id (a group of one output authorised by the
entry input), so no existing covenant can be passed off as the exit.

- **Buy-first (side BID).** Holds a B escrow custody (`custody`, exact; no custody UTXO when 0) and buys A: a fill releases
  at most `⌊n·p / scale(A)⌋` of B (the rest stays in the custody at its index), delivers exactly n of A at output i owned
  by the exit's covenant id, and creates the exit `KobCondPair` side ASK with `custody := n`. The builders refuse an
  escrow below the spend of the whole amount at `price`.
- **Sell-first (side ASK).** Holds an A custody (exactly `amountLeft`; none when 0) and a **B prefund custody** (`custody`,
  exact; none when 0) and sells A: a fill releases n of A and puts `tOut + pre(n)` of B at output i, owned by the exit's
  covenant id, where `tOut ≥ ⌈n·p / scale(A)⌉` is the proceeds (the filler's argument, written into the exit's state)
  and `pre(n) = ⌈n·prefund / scale(A)⌉` comes from the prefund custody (the rest stays at its index). The exit is a
  `KobCondPair` side BID with `custody := tOut + pre(n)`: it buys A back with its proceeds and the prefund, so `prefund`
  (B per whole A) must cover the exit's worst buy-back beyond the entry price (the builders require `price + prefund ≥`
  the exit's worst leg, per whole A). The prefund custody is funded with `⌈amount·prefund / scale(A)⌉` plus one base unit
  per possible fill but the last (each fill takes its ceil); when a non-repeating entry sells out, the last exit takes
  the whole prefund left.
- **Entry price.** A limit entry (`entryStop = 0`) trades at `price`. A stop entry is armed by trigger evidence (BID: a
  buy stop, the rate rose to `≥ entryStop`; ASK: a sell stop, the rate fell to `≤ entryStop`; *Pair trigger rule*), in
  its fill or by `update`, then auctions from `entryStop` to `price` over `bandDaa` (BID `entryStop ≤ price`, ASK
  `entryStop ≥ price`).
- **KAS.** Per fill the entry pays `deliveryCarrier` (on the exit's custody), `exitCarrier` (on the exit UTXO) and the
  tip `⌊n·tip / scale(A)⌋`; the wallet funds `⌈amount / minFill⌉ × (deliveryCarrier + exitCarrier)`, the tip of the
  whole amount and, for a repeating entry, one more `exitCarrier` (a merge's new custody).
- **Refund** (`fill(0)`, anyone after `expiryDaa` or 90 days idle): each custody back to the maker at its own index with
  its carrier, the entry's KAS to the maker at output i (minus `refundTip`). It also ends an empty repeating entry (there
  is no separate `close`).

**Repeat (pair).** As the KAS kinds (*Repeat IFD in detail*), with the tip outside the rates (`rptPrice = price`, a
sell-first exit's `rptPre = prefund`). Per take-profit of m base units of A, every amount rounded in the maker's favour:

| | Buy-first (entry BID, exit ASK) | Sell-first (entry ASK, exit BID) |
|---|---|---|
| entry fill | pays at most `⌊n·p / scale(A)⌋` of B; the exit gets n of A | receives at least `⌈n·p / scale(A)⌉` of B; the exit gets it plus `⌈n·prefund / scale(A)⌉` |
| take-profit | the exit sells m of A at `≥ tp` | the exit buys m of A back at `≤ tp` |
| maker receives (B, at the exit's output i) | `tOut − ⌈m·price / scale(A)⌉ > 0` | `⌈m·price / scale(A)⌉ − sOut > 0` (on the exit's sell-out: its custody less the prefund back and `sOut`) |
| entry gets back | its B escrow grows by exactly `⌈m·price / scale(A)⌉` (a NEW escrow custody when it held none) | the m bought-back A join its A custody (exactly `amountLeft + m`; new when none) and its prefund custody grows by exactly `⌈m·prefund / scale(A)⌉` (new when none) |

A new custody carries `exitCarrier` from the entry UTXO; when the exit sells out, its KAS (its UTXO and its custody's
carrier) goes to the entry minus its tip and the carrier of the maker's profit output. The entry accepts only an exit
exactly as it books it (the committed `exitState` outside the mutable window, `parent` = the entry, `rptPrice = price`,
`rptPre`, the opposite side at the entry's scale of A) and only from that exit's take-profit fill of m (each side
checks the other's leading 8-byte push); a merge into an armed, not yet filled entry fixes the auction origin, a merge
into an entry with nothing left resets `armed` to 0.

### Pair trigger rule

A pair stop (a `KobCondPair` stop leg or trailing ratchet, a `KobIfdPair` stop entry) changes state only on trigger
evidence filled in the same transaction, in one of two modes (argument `evMode`). Only quotes that cost money to fake
count, and the order's own counterparties are never its evidence (the side rule).

**Mode 0, two KAS books.** Two plain resting KAS-book orders filled in this transaction (8-byte first push `n > 0`),
one of A (`evA`) and one of B (`evB`), each authenticated by template hash (`KobAsk` / `KobBid` of that token's family),
with `slope = 0`, a scale equal to the token's scale, exposed at its quote for at least `minRestDaa` DAA
(`matcher.md` §4.2: `max(UTXO DAA + interval, activeFrom, custody DAA)` for each), `n_A ≥ minTouch` (base units of A) and
`n_B ≥ ⌈minTouch·X / scale(A)⌉` (base units of B: the B value of the threshold at the order's current stop X, `stopPrice`
or `entryStop`). With their quotes `a` (sompi per whole A) and `b` (sompi per whole B), the implied rate is
`r = a·scale(B) / b` B base units per whole A. Every comparison is exact, through `quoteOf` with the denominator
`scale(B)` (no 128-bit arithmetic; `a` is an integer, so `a ≤ ⌊x·b / scale(B)⌋ ⇔ r ≤ x` and `a ≥ ⌈x·b / scale(B)⌉ ⇔
r ≥ x`):

| Order | Evidence (both filled) | Condition |
|---|---|---|
| sell stop arms (`KobCondPair` ASK, `KobIfdPair` ASK stop entry) | an **ask** of A at `a`, a **bid** of B at `b` | `a ≤ ⌊stop·b / scale(B)⌋` |
| buy stop arms (`KobCondPair` BID, `KobIfdPair` BID stop entry) | a **bid** of A at `a`, an **ask** of B at `b` | `a ≥ ⌈stop·b / scale(B)⌉` |
| trailing sell stop ratchets up to `s′ = stop + k·step` (`KobCondPair` ASK) | a **bid** of A, an **ask** of B | valid: `a ≥ ⌈(s′ + gap)·b / scale(B)⌉` and `s′ < tpPrice` (when `tpPrice > 0`); maximal: `a < ⌈(s′ + step + gap)·b / scale(B)⌉`, or `s′ + step ≥ tpPrice` (`tpPrice > 0`) |
| trailing buy stop ratchets down to `s′ = stop − k·step` (`KobCondPair` BID) | an **ask** of A, a **bid** of B | valid: `s′ > max(tpPrice, 0)`, `s′ − gap ≥ 0` and `a ≤ ⌊(s′ − gap)·b / scale(B)⌋`; maximal: `s′ − step ≤ max(tpPrice, 0)`, or `s′ − step − gap < 0`, or `a > ⌊(s′ − step − gap)·b / scale(B)⌋` |

A sell stop needs cheap A offered and dear B bid (the rate fell); a buy stop the reverse. The filler supplies k ≥ 1 for
a ratchet; the covenant accepts only the valid and maximal k, at most once per `trailWait` DAA, and only on an unarmed
stop.

**Mode 1, a resting pair order.** One resting `KobPair` of THIS pair (the same A and B, the same scales) filled in this
transaction (`evA`; `evB` unused), with `slope = 0`, exposed for at least `minRestDaa` (`max(its UTXO DAA + interval,
activeFrom, its custody's DAA)`), `n ≥ minTouch`. Its price `rp` is the rate: a sell stop arms on a pair ASK with
`rp ≤ stop`, a buy stop on a pair BID with `rp ≥ stop`; a trailing sell stop ratchets on a pair BID, a trailing buy stop
on a pair ASK, with the comparisons above read with `a = rp`, `b = scale(B)` (plain comparisons with `rp`). A `KobPair`
fill is evidence because every `KobPair` quote is takeable (*Plain pair orders*). Its price never enters the price
record (`matcher.md` §3.5).

The evidence is read, never consumed: one fill (or pair of fills) may arm any number of pair stops. Conditional, if-done
and pair fills are never evidence of a KAS-quoted order; a KAS-quoted conditional keeps its KAS-book evidence only.

### Pair view

In the pair view A/B a pair ASK is an ask at `price / scale(B)` whole B per whole A (a decaying ask at its current
price), a pair BID a bid; the route levels implied by the two KAS books (the best bid of A against the best ask of B for
a sell of A, and the reverse for a buy) are shown next to them (`matcher.md` §3.5). Pair charts are derived from the two
KAS price series; a netted or inventory pair fill shows as volume only.

## Defaults (wallet)

| Parameter | Default |
|---|---|
| tip | 0 |
| minimum fill (`minFill`) | the amount worth 10 KAS (`DEFAULT_MIN_FILL_SOMPI`, a notional, not a carrier) at the limit price, at least 1 base unit, at most the amount (a pair order: the amount of A worth 10 KAS on A's KAS book, `default_min_fill_pair`; `⌈amount / 4⌉` without a KAS quote of A); IOC / FOK / market orders 1 base unit; if-done entries `⌈amount / 4⌉` |
| market auction (token and pair) | 3% bound over 200 DAA (20 s), activation +30 DAA, IOC life 300 DAA |
| stop band / auction | 3% (`slipBps` 300) over 300 DAA (30 s) |
| trigger rest / threshold (per order, the wallet exposes both) | 5 s (`minRestDaa` 50) / `minTouch` = the order's own `minFill` (presets: min fill / 25% / 50% / 100% of the order's own amount, or custom); a pair stop in mode 0 also needs `⌈minTouch·stop / scale(A)⌉` of B traded |
| keeper tip (arm / trail, taken by the arming matcher) | to be measured (pair phase) (`data/keeper_tips.json`, `matcher.md` §10.6) |
| refund tip | per token program (a pair order: the larger of its two programs): to be measured (pair phase) (`data/keeper_tips.json`, `matcher.md` §5) |
| day order | until 00:00 UTC |
| GTC | 90 days idle (renew before day 85) |
| carriers | 2 KAS per covenant UTXO (`DEFAULT_ORDER_CARRIER`; a pair entry's `exitCarrier` also funds its exit's deliveries, `IfdPairState::exit_carrier_needed`): the relay fee of no transaction shape rises below 10 KAS (storage mass is not part of it), and at 2 KAS the largest shape commits 35% of the block storage limit (1 KAS: 71%; 0.5 KAS refuses pair fills with evidence or netting); the opt-in storage-inclusive priority fee rises by at most 0.13 KAS (`tests/carrier_fees.rs`). Never below the floor of a token output (KaspaCom KCC20 0.2.5: 0.5 KAS; every other supported program: none) nor the KIP-9 dust bound of 0.02 KAS (`TemplateId::min_token_output`, `tx::DUST_OUTPUT_MIN`): orders and builders refuse less |
| pair orders | the typed amount of A exactly (no rounding of the size); `price` = the displayed price (B per A) in base units of B per whole A, rounded in the maker's favour (an ask UP, a bid DOWN); `tip` 0 (KAS, sompi per whole A, prefunded); `deliveryCarrier` (at least the floor of a token output of the token delivered) prefunded for `⌈amount / minFill⌉` fills (unused carriers return with the last fill, the refund or the cancel); a bid's escrow and a sell-first entry's prefund as above; pair market order: a `KobPair` IOC auction from the touch (the best of the opposite pair orders and the route level of the two KAS books) to the touch ∓ 3% over 200 DAA (`matcher.md` §10.14) |
