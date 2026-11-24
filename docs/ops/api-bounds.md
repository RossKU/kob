# Bounds of the public read API

Every public route of the read API (`kobd --indexer`, `docs/api-reference.md`), what one computation of it reads at most, and
the test that measures it. The tests seed the rows an attacker can add cheaply, run the route's read with N and then 2 N of them
(both past the route's bound), and assert that the SQLite VM steps of the read do not grow with N (within 5 % and 500 steps),
and that the plans of the statements that serve it have no table scan and no temporary B-tree over those rows. The VM steps are
counted by a progress handler over every statement the read runs (`crates/kobd/src/indexer/bounds_tests.rs`, `steps`). The
x402 routes are not part of this API.

Lanes: **L** light (one connection, one read per client at a time), **H** heavy (`read_pool_size`, 4 connections, two reads per
client), **M** memory only. Cache: **G** per data generation (the store's commits), **S** per generation and wall-clock second
(and the node's DAA score), **P** per parameters the client chooses, **U** uncached (the CDN keeps `public, max-age` answers, see
`docs/api-reference.md` 5.7). Test names are in `crates/kobd/src/indexer/bounds_tests.rs` (`bounds`) and
`crates/kobd/src/api/tests.rs` (`api`).

| Route | Lane | Cache | Worst case one computation reads | Test (VM steps measured with N / 2 N rows) |
|---|---|---|---|---|
| `/v1/health`, `/v1/health/ready`, `/v1/metrics` | M | the counters at most once a second, one light read outside any client | nothing per request; outside the concurrency cap | `api::health_answers_while_every_request_permit_is_taken`, `api::health_answers_while_the_light_lane_is_busy` |
| `/v1/tokens` | H | G | per listed token, two counts of at most 1,400 fillable `book_live` rows; the open list at most 5,000 rows (`OPEN_TOKEN_ROWS_MAX`) | `bounds::the_book_reads_do_not_grow_with_listed_curve_asks` (tokens: 93,611 / 93,611) |
| `/v1/tokens/{id}/declaration` | L | U | one primary-key lookup and two transactions decoded | `bounds::token_events_and_a_declaration_do_not_grow_with_other_tokens` (declaration: 9 / 9) |
| `/v1/books/{token}` | H | S, P (`depth`, `aggregate`) | per side the best 1,000 fillable orders with a fixed quote (`BOOK_ORDERS_MAX`), 200 fillable auctions and 200 opened since the last commit (`BOOK_MOVING_MAX`); orders that cannot be filled now are not read | `bounds::the_book_reads_do_not_grow_with_listed_curve_asks` (12,000 / 24,000 curve asks; aggregate: 214,724 / 214,724; orders: 207,325 / 207,325); `bounds::worst_case_book_reads_over_1000_level_curves` (every range full of 1,000-level curves, N / 2 N: orders 249,385 / 253,635, with dust 287,448 / 291,698; the growth is the capped `dust_orders` count); `bounds::orders_that_cannot_be_filled_now_take_no_place_in_the_book` (honest book alone, then with 12,000 not-yet-active, 12,000 between TWAP slices and 12,000 expired asks at a better price: 5,975 / 5,976) |
| `/v1/depth/{token}` | H | S, P | one book read | same (depth: 214,423 / 214,423; with the junk: 5,984 / 5,985); a single book read of `levels` prices since R7C2b-3 |
| `/v1/stats/{token}` | H | S | four side reads as the book's; at most 20,000 priced fills of the 24 h window (`MAX_FILL_ROWS`, rows read through `order_events_priced_ts`) | same (stats: 404,576 / 404,576; with the junk: 12,105 / 12,000); `bounds::the_market_reads_do_not_grow_with_fills` (25,000 and 50,000 fills: 633,150 / 633,030) |
| `/v1/stats?since=` | H | G, P (minute) | the activity buckets of the range, the locked KAS from one counter row (`counters.kas_locked`, R7C2b-3), and the makers and executors active since the range's first block (`actors`, one per key that placed or paid: bounded by the read budget) | `bounds::the_market_reads_do_not_grow_with_fills` (activity: 115 / 115) |
| `/v1/pairs?token=` | H | S | the oldest 20,000 fillable `pair_live` rows (`PAIR_SCAN_MAX`; with `token`, that many as base and as quote) | `bounds::the_pair_reads_do_not_grow_with_pair_orders` (rows the writer set: see the wall-time table below; an older build's unset rows, read row by row: 6,380,049 / 6,380,040) |
| `/v1/pairs/{a}/{b}/book` | H | S, P | 20,001 fillable `pair_live` rows of the pair, at most 2,000 resting, and 200 opened since the last commit; the KAS route of each token as the book's rows | same (unset rows: 6,220,179 / 6,220,179; 20,001 real fillable orders: 62,183, `bounds::worst_case_pair_reads_over_real_pair_orders`); `bounds::the_book_reads_do_not_grow_with_listed_curve_asks` (pair book, KAS route: 605 / 605) |
| `/v1/pairs/{a}/{b}/candles` | H | P | the two tokens' candle rows of the window (`limit` at most 1,500) and one `pair_volume` row per shown bucket (the writer's aggregates, R5C2-6); no pair fill | `bounds::the_pair_candles_read_one_volume_row_a_bucket` (210,000 / 420,000 fills in the shown buckets: 1,569 / 1,463) |
| `/v1/pairs/{a}/{b}/fills` | H | U | a page (`limit` + 1) and at most 20,001 fills of the 24 h window | `bounds::the_pair_reads_do_not_grow_with_pair_orders` (pair fills: 201,253 / 201,210) |
| `/v1/orders?maker=&token=&status=` | H | S, P | a page of at most `max_page_size` + 1 orders, each with its custody (2 rows), at most 101 strays and a count of at most 10,000 index entries; a status read in history order reads at most 10,000 orders of the history (`ORDERS_SCAN_MAX`; a short page carries the cursor, R6C2b-2) | `bounds::the_order_view_does_not_grow_with_children_strays_or_spent_custody` (orders of a maker: 138,575 / 138,575) |
| `/v1/orders/{id}` | L | U | the order, at most 101 children and 101 live strays, two counts of at most 10,000 index entries each (`VIEW_COUNT_MAX`, index only), the custody and the bought-custody (2 rows each, live partial indexes) | same (order view: 220,489 / 220,489; bought custody: 91 / 91) |
| `/v1/orders/{id}/events` | L | U | `limit` + 1 events (at most 201) | same (order events: 3,398 / 3,398) |
| `/v1/fills` | H | S, P | `limit` + 1 fills | `bounds::the_market_reads_do_not_grow_with_fills` (fills: 1,545 / 1,545) |
| `/v1/trades/{token}` | H | U | `limit` trades of fills (the over-fetch of their transactions) | same (trades: 38,320 / 38,320); `bounds::self_takes_do_not_grow_the_market_reads` (30,000 / 60,000 self-takes: trades 220 / 220, stats 13,150 / 13,030, pair-candle carry 83 / 83, lean candles 339 / 339) |
| `/v1/candles/{token}` | H | P | the candle rows of the window, at most `limit` (1,500) | same (candles: 120 / 121) |
| `/v1/quotes/{token}` | H | P | the `book_quotes` rows of the last `limit` intervals of the window: at `1d` x 1,500 that is the whole 30-day retention, at most 43,200 minute rows of one token (R7C2b-3), within the read budget | same (quotes: 58 / 58) |
| `/v1/strays?maker=` | H | U | `limit` live strays, newest first by index; with `maker`, at most the newest 10,000 live strays (`STRAYS_SCAN_MAX`) | `bounds::strays_and_an_owners_holdings_of_a_token_do_not_grow` (strays: 4,696 / 4,696; of a maker: 150,072 / 150,072) |
| `/v1/token-events?token=` | H | U | `limit` events by index | `bounds::token_events_and_a_declaration_do_not_grow_with_other_tokens` (of a token: 13 / 13; all: 459 / 459) |
| `/v1/token-utxos?owner=&token=&spent=` | H | U | `limit` + 1 holdings by a page index; with `owner` and `token`, at most 10,000 of the owner's holdings and the rest of one block (`HOLDINGS_SCAN_MAX`; a short page carries the cursor) | `bounds::strays_and_an_owners_holdings_of_a_token_do_not_grow` (owner, token, spent: 110,149 / 110,149; owner, token: 156 / 156) |
| `/v1/creator-locks?key=` | H | U | `limit` + 64 lock index rows (`CREATOR_LOCKS_SLACK`), verified once per declaration (cache of 4,096) | by reading (index order, `LIMIT`; security review 2026-11-19 K-L5) |
| `/v1/creator-locks?due=` | H | U | at most 200 automatic locks still holding (`DUE_LOCKS_MAX`, `kcc23_auto_live`); the released ones are not read | `bounds::the_due_creator_locks_do_not_grow_with_released_ones` (5,000 / 10,000 released locks: 503 / 476) |
| `/v1/logo-blocklist`, `GET /v1/logo-votes` | M, its own connection | 5 s | one row per hidden or voted token, once per 5 s for everyone; not one per report or vote | `bounds::the_logo_lists_do_not_grow_with_votes_or_reports` (5,000 / 10,000 votes and reports on 100 tokens: votes 1,319 / 1,319; blocklist 920 / 920) |
| `POST /v1/logo-reports`, `POST /v1/logo-votes` | its own connection | none | bounded by the request; votes are proved by `logo_votes` tests (`held_since_daa`, VM steps) | `logo_votes::tests` |
| `/v1/price/kas-usd` | M | the quote the indexer fetched | nothing from the store | none needed |
| `/v1/ws` | M | none | no read; per commit, the subscriptions times the fills (capped per address and in all) | `api::websocket_end_to_end` |

The logo lists grow with the number of tokens hidden or voted on: a vote needs a token, an address with history and a proof of
work, a hidden logo a report within the daily report budget. They are one list each, the same for every viewer, kept 5 s.

## Wall time and distinct computations, at the worst case (R5C2-6)

The worst case of a route is read with the attacker's own token or pair, where "real value on offer" costs him only carriers
and fees. Measured on the build PC (Windows), **test profile** (`profile.dev`, `opt-level = 1`; release not measured), one
computation, store in memory, by `cargo test -p kobd --lib bounds_tests -- --include-ignored --nocapture --test-threads 1`. The
wall time includes the VM-step counter (a callback at every SQLite instruction), so it is an upper bound. "Keys" is how many
distinct computations one client can cause per data generation (and second, for S): the rest are answered from the cache.

| Route | Worst case seeded | Wall | VM steps | Keys a client can cause |
|---|---|---|---|---|
| `/v1/tokens` | 24,000 curve asks on one token | 0.7 ms | 31,114 | 1 |
| `/v1/books/{token}?aggregate=false&depth=200` | 1,200 curve asks of 1,000 levels at the best price, 250 dust and 250 pending curves of 1,000 levels (2,400 / 500 / 500 for the 2 N run); with `include_dust` | 18 ms; 24 ms (re-measured 2026-11-24, R7) | 253,635; 291,698 | 16 per token and second: `depth` is computed at 20, 50, 100 or 200 (`api::rest::DEPTH_STEPS`) and cut, times `aggregate`, times `include_dust` |
| `/v1/books/{token}` (levels, depth 200, `include_dust`) | the same | 34 ms | 297,362 | (above) |
| `/v1/depth/{token}?levels=200&include_dust=true` | the same | 35 ms (no re-read at 4 x the levels since R7C2b-3: when two scales round to one per-basis level the worst level is left out and the answer says `truncated`) | 297,066 | 8 per token and second (the same steps, times `include_dust`) |
| `/v1/stats/{token}` | the same; and 25,000 / 50,000 fills of the day (each read alone; one token holding both is about the sum, within the read budget) | 57 ms; 63 ms | 478,596; 633,030 | 1 per token and second |
| `/v1/quotes` sampling (once a minute) | 12,000 / 24,000 curve asks | 5 ms | 199,512 | none (the writer's) |
| `/v1/pairs?token=` | 20,001 fillable `KobPair` asks, real states and custodies | 25 ms | 780,122 | 1 per token and second |
| `/v1/pairs/{a}/{b}/book?depth=` | the same | 1.9 ms | 62,183 | 4 per pair and second (the steps) |
| `/v1/pairs/{a}/{b}/candles` | 420,000 pair fills in the shown buckets | 1.1 ms (was 226 ms) | 1,463 | one per `interval`, `from`, `to`, `limit`; each at most `limit` candle rows of each token and `limit` volume rows |
| `/v1/pairs/{a}/{b}/fills` | 24 h of fills (20,001 read) | 19 ms | 201,210 | one per request (uncached) |
| `/v1/orders?maker=` | a maker's page at its bound | 2.6 ms | 138,572 | one per request parameters and second |
| `/v1/orders?status=cancelled` | 2,000 old cancelled orders behind 30,000 / 60,000 newer filled ones (history order) | 6.4 ms | 211,210 | one per request parameters and second |
| `/v1/candles/{token}` on a lean store | 35,000 / 70,000 fills in the window (30,000 read) | 82 ms | 930,338 | one per bucket edge, `interval`, `limit` |
| `/v1/orders/{id}` | 10,000 children and strays | 3.8 ms | 220,489 | one per request (light lane, uncached) |
| `/v1/orders/{id}/events` | 201 events | 0.3 ms | 3,398 | one per request |
| `/v1/fills`, `/v1/trades/{token}` | 50,000 fills | 0.3 ms; 2.0 ms | 1,545; 38,320 | one per parameters (and second) |
| `/v1/candles/{token}`, `/v1/quotes/{token}` | 1,500 rows | 0.1 ms | 121; 58 | one per parameters: not bounded, each tens of microseconds |
| `/v1/stats?since=` | the activity buckets, one counter row | 0.1 ms | 115 | one per minute value |
| `/v1/strays?maker=` | 10,000 live strays of the maker | 2.8 ms | 150,072 | one per request |
| `/v1/token-utxos?owner=&token=&spent=` | the owner's 10,000 holdings of other tokens | 1.4 ms | 110,149 | one per request |
| `/v1/creator-locks?due=`, `/v1/token-events`, the logo lists, the declaration | as in the table above | under 0.2 ms | under 1,400 | |

What changed to get there (R5C2-6): a book read decodes at most `depth` orders of each range ranked by key (the rest of the
range can only tie the worst level kept), the aggregated levels and the statistics stop at the first row worse than what they
need, and the pair reads take each order from its `pair_live` row as the writer decided it (side, source, scale, the amount one
fill can take, its quote as a function of the DAA score) instead of decoding its state and reading its custody.

No public route is above 100 ms at its worst case (re-measured 2026-11-24, R6C2b-2; the book worst case is `bounds::worst_case_book_reads_over_1000_level_curves`, now a non-ignored test with a VM-steps assertion). A maker page of 200 live orders each with 10,000 live strays is not seeded: each order's view is measured at 3.8 ms with 10,000 strays, so such a page would take about 0.8 s, but it needs 2,000,000 stray token UTXOs of at least `COVENANT_OUTPUT_MIN` (0.08 KAS) each, about 1.6·10^5 KAS given to the maker, so it is left as it is. The pair candles take their volume from `pair_volume`: per pair and candle
bucket of each interval served (1 m, 5 m, 1 h, 1 d), kept by triggers on `pair_fills` and the writer's commit
(`indexer::pair_volume`), so a busy pair's volume on a long window is never cut. Each pair fill adds at most 4 rows (its
bucket of each interval, shared by the fills of that bucket) and one transient mark; the first open over 100,000 pair fills
builds them in 0.26 s (37,540 rows over 30,834 minutes; `pair_volume::tests::pair_volume_first_open_time`).

The chart routes (`/v1/candles`, `/v1/quotes`, the pair candles) answer in whole buckets (R6C2b-1): `from` is moved down and
`to` up to a bucket start of the interval before the cache key is made and the store read, so the window's edges never fall
inside a bucket and a chart reads the aggregate rows of its buckets, never the fills of an edge bucket
(`bounds::candle_edges_do_not_grow_with_the_fills_of_a_day`: 50,000 / 100,000 fills in the edge day cost the same steps). The
keys are then one per bucket edge, `interval` and `limit`, each at most `limit` (1,500) rows, about a millisecond. A store that
keeps no candle aggregates (a lean store, `--lean`) charts from the fills: at most the newest 30,000 of the window
(`market::CANDLE_SCAN_ROWS_MAX`, about 50 ms), then `truncated` (`bounds::lean_candles_read_at_most_their_cap_of_fills`).
`/v1/quotes` reads at most the last `limit` intervals of its window (an earlier `from` is moved up to them).

A row the writer has not set yet (`ok` NULL: an older build's write to the store) is decided row by row, as the writer would;
that costs about 90 microseconds a pair order (`bounds::the_pair_reads_do_not_grow_with_pair_orders`, 1.8 s for 20,001). This
build sets every row at its open, before the API listens, and in every commit, so the API never meets such rows.

## What a book read takes

The writer keeps, for every live listed order of a KAS book (`book_live`) or of a pair (`pair_live`), the window in which its
current state can be filled (`db::maintain_live_books`, in every commit): from `activeFrom` and, for a TWAP or DCA order, its
UTXO's DAA score plus `interval` (the slice), to its expiry, its soft expiry (90 days idle; not for a curve) and its day-order
deadline; and whether it can be filled at all as it stands (exact custody, not possibly frozen, a known state with an amount
left, a resting GTC kind, a pinned template, an armed entry, a curve not sold out). A row moves in or out of the range the reads
take (`open` = 1) when its window opens or closes; that work is the rows that change, through the `(open, open_from)`,
`(open, closed_from)` and `(open, deadline)` indexes, never the rows that wait. The reads take the best rows of that range by
key (price per base unit; a curve by its current level, a bid negated) through `book_live_key`, so an order that cannot be filled
now is outside the index range read: it costs nothing and takes no place.

Auction orders (a decaying ask, a rising bid, an armed entry) are ranked by their limit, which is not their quote, so they are
read apart, at most 200 a side: they cannot take the places of the orders with a fixed quote. Orders whose window opened after the
store's last commit and by the node's DAA score are read apart too, at most 200.

**The rule of the places (R5A-1, R5C2-1, R6C2a-1, R6C2a-2, R7C2a-1..3):** a place among the best 1,000 of a side is taken only by an order
that offers a taker at least half the listing minimum (`min_order_value_sompi` / 2, 0.5 KAS by default) of real value right now.
Real value is what one fill (one transaction) can take from the order now, and what the taker pays or gets for it, by the
matcher's own fill rules for that order (`matcher::candidate::candidates_of`, `matcher::pair::infos`, called by
`indexer::takeable`): every fill condition of its covenant (the minimum and maximum fill against the amount left, the tip
against the quote, a curve's levels and its walk of at most 8 of them, the escrow, prefund and carriers) and the KIP-9 minimum of
every output the fill builds. It is the least of what it offers at both ends of the order's window (its first DAA from the
commit on, and its last): every kind's offer is monotone over the window, so that is its least favourable moment (a decaying ask
at its floor; a rising bid at its first quote, with the buying power of its highest; an armed entry at the band's end it starts
from). A curve is ranked, shown and valued by its smallest valid fill (its minimum fill as KIP-9 raises it) at the mean price a
taker pays for it, then its other reachable levels, never below an earlier one. A pair order shows the least amount one fill can
take at either end of its window. `bounds::a_rising_bid_and_a_bundled_curve_do_not_set_the_touch` is the findings' own case and
`book_rule_props::the_book_rule_holds_for_every_kind_at_every_time_of_its_window` the property over every kind (below). An order no fill can take takes no place
at all (`ok` 0); one worth less than the threshold (an order its own maker filled down to one base unit, a TWAP slice or a
curve's reachable levels worth less, an auction whose floor is worth less) is **dust**: it stays listed, executors fill it, and
the reads take it from a range of its own of at most 200 (`auction` = 2), never from the 1,000. `/v1/books` and `/v1/depth`
leave dust out unless `include_dust=true` (then marked) and say how much (`dust_orders`); the 24 h statistics' best bid and ask
(and so mid, spread and the 2 % depth), the sampled quotes of `/v1/quotes`, the KAS legs of a pair book's route and the
`/v1/tokens` counts read no dust at all. Why half: the listing rule measures a bid by its whole escrow, carriers included, and
real value counts only what the escrow buys, so an honest bid placed at exactly the listing minimum offers a little less than
the minimum; half keeps every order an honest builder (the web, `kob place`) places at the minimum out of the dust range
(`bounds::an_honest_minimum_order_is_not_dust`).

**What an attacker must do now to push a real order out of a book read**, per kind on the side (each listed, active, not expired,
a resting kind with an open slice):

| Kind | What each of the 1,000 must hold | Who can take it |
|---|---|---|
| `KobAsk`, `KobAskKron`, `KobIfdAsk` (+ KRON) | tokens worth at least 0.5 KAS at its price (an auction: at its floor) in its exact custody (a TWAP ask: per slice); a sell-first entry also the KAS of the prefund and exit carrier of a partial fill, else it can only be taken whole | anyone, at once, at the price shown |
| `KobCurveAsk` | tokens worth at least 0.5 KAS over the levels one fill reaches, ranked at the mean price of its smallest valid fill | anyone, at once, at that mean price for the smallest fill |
| `KobBid`, `KobBidKron` | at least 0.5 KAS of buying power at its lowest quote of the window (a DCA bid: per slice) above the carriers and reserve | anyone, at once, at the price shown |
| `KobIfdBid` (+ KRON) | at least 0.5 KAS that one fill can spend from its escrow after its delivery and exit carriers, at both ends of its band | anyone, at once, at the price shown |
| unarmed stops, conditionals (`KobCond*`), IOC / FOK takers | not in the books | |

That is at least 500 KAS of tokens (asks) or KAS (bids) really on offer, which anyone can take at once (and each order must also be listed: worth the whole listing minimum by the listing rule). Auctions and dust take
at most their own 200 places each. Orders not active yet, between slices, expired, past their deadline, without custody, possibly
frozen or unable to fund a fill take none, whatever their number or price.

The pair book reads the 20,001 oldest fillable pair orders. Pair orders have no KAS listing minimum (a pair order has no KAS
quote), so there is no dust class there: each must fund one fill (a `KobPair` bid or a buy-first `KobIfdPair` with its B custody,
a sell-first one with its prefund, the carriers and tip in KAS) and have something left (R5A-1, R5A-2), but it may be small; the
cost of filling the 20,001 is their carriers, about 0.16 KAS each, recoverable (accepted, R5A-2).

Rows an older build (schema 14) writes through the triggers have no window yet (`ok` NULL): they count as fillable and are
decided row by row, within the same bounds, until this build's next commit or start sets them. Running an earlier build on a
store this build opened is not supported: rows it decides itself keep its decision (`docs/releases/mainnet-0.2.0.md`, first
start); deleting `meta.book_live_rules` makes the next start decide every row again.

## The read budget (R7, A)

Every statement of every public read runs under a budget of work, whatever its plan and whatever rows an attacker wrote: at most
**5,000,000 SQLite VM steps** (`db::READ_BUDGET_STEPS`) and **1.5 s of read time** (`db::READ_BUDGET_WALL`, the Rust work between
the statements included), counted per request over all its statements (a computation of the response cache, detached from its
request, has a budget of its own). The progress handler of the read pool's connections checks it every 1,000 steps
(`db::READ_BUDGET_TICK`); the pool serves the public API only, so the writer, the executor, the follower and
`maintain_live_books`, which read their own connections, are exempt. A read past it stops at once and fails for that request only:
`503` with code `read_budget` and `Retry-After: 1`, never cached as a success; a cache key whose computation hit it is refused
for a second without a new computation (`rest::READ_BUDGET_REFUSED_FOR`), and `/v1/metrics` counts the refusals by route
(`kob_api_read_budget_refused_total{route}`). The largest honest worst case measured below is 0.93 million steps (a lean store's
chart over 30,000 fills; 1.4 million before the fills carried their priced flag), so the budget is about 5 times it; the per-route caps stay what keeps honest answers complete.
`api::every_public_read_ends_at_the_read_budget` seeds 10^6 fills (half of them self-takes), 10^6 junk orders, 10^6 strays of
one order, 10^6 pair fills and 10^6 token events and reads every public route: each answers (200, 404) or refuses with
`read_budget`, in well under a second; with a budget below any real read every route that reads is refused.

**What a read costs an attacker now.** At the test profile's 17 to 60 ns a step, a read is at most about 0.1 to 0.3 s of one heavy
connection, and at most 1.5 s of read time (the wall bound). There are 4 heavy connections and each client key holds at most
2 of them (`CLIENT_HEAVY_READS`), FIFO. To keep all 4 busy an attacker needs 2 client keys whose every read hits the budget;
an honest heavy read then waits behind at most the reads queued ahead of it: with K attacking keys (2 reads each, T = 0.3 s each,
at most 1.5 s) about (2K / 4) x T. The request timeout is 10 s: K = 64 keys at T = 0.3 s (or 13 at the 1.5 s wall bound, which
needs reads that spend their time outside SQLite) push an honest heavy read past it. Filling the 256 server permits takes 32 IPv4
addresses or 8 IPv6 /48s anyway (below), so the read budget turns "a few addresses stop the heavy lane" (R6C2b-1, R7C2b-1: 4 to 12
keys at 2 to 8 s a read) into the same order as the request-permit bound. Honest readers see their heavy reads queue for at most
the reads ahead of them, each at most the budget; light reads (an order, a declaration) keep their own connection; `/v1/health`
takes none.

## What fills the request permits

The server keeps at most `max_concurrent_requests` (256) requests in flight. One client key (an IPv4 address, an IPv6 /64) keeps at
most 8 (`CLIENT_IN_FLIGHT`), the /64s of one IPv6 /48 at most 32 together (`CLIENT_SITE_IN_FLIGHT`). A request past them waits
for one, at most 2 s (`CLIENT_QUEUE_WAIT`), and holds no server permit while it waits; at most 24 of a client (`CLIENT_QUEUED`)
and 96 of a site (`CLIENT_SITE_QUEUED`) wait, and one more is answered `503 overloaded` at once (R5B-L1: one honest page sends
12 to 20 reads at once, and the web, the Android app and other clients keep doing so). A read that waits for its own client's lane
permit (1 light, 2 heavy) gives up after 2 s (`CLIENT_WAIT`) with `503 overloaded`; it is sized for a burst (6 reads behind the
2 heavy ones: 3 in turn, 0.2 s at the 70 ms of `/v1/stats` and 1.8 s at the 0.6 s of a 500-candle chart on the TN10 soak copy;
6 order views of one book check on the single light permit, 5 in turn at 3.5 ms each at the view's bound, measured in the test profile) and does not change what one client
holds: at most 8 server permits. So filling the 256 takes 32 IPv4 addresses or 8 IPv6 /48s, each keeping 8 requests in flight
(at the per-client rate limit of 20 a second); `api::one_client_holds_at_most_its_in_flight_server_permits` and
`api::the_64s_of_one_ipv6_site_share_its_requests_in_flight` prove the per-key bounds, `api::an_honest_pages_burst_is_served_whole`
the burst. The others then get `503 overloaded` with `Retry-After: 1`; `/v1/health`,
`/v1/health/ready` and `/v1/metrics` take no permit and keep answering. A response the cache shares is computed under the
permit of the client that starts it, taken before the computation exists, so the other viewers of a hot key never wait for that
client's queue (R4C2-3).

## Start-up work that grows with attacker rows

At each writer open, `db::backfill_stop_entry_books` re-derives every live if-done entry in a book and
`db::backfill_exit_extensions` walks the if-done exits without an extension commitment. Both are idempotent and run every open on
purpose: they repair rows an earlier build wrote, including after a downgrade and upgrade. Their cost is linear in live if-done
entries (each locks its order value) and in exits (one per fill of an entry), seconds per 10^5 rows, at start-up only, before the
API listens. They are not reachable from the API. `db::backfill_book_live` and `db::backfill_auto_live` fill `book_live`,
`pair_live` and `kcc23_auto_live` once (`meta.book_live`, `meta.kcc23_auto_live`): one pass over the live orders and the
automatic locks; then `db::maintain_live_books` sets the windows of the rows not set yet, in one transaction (R5F-2: 22 s for
100,000 live listed orders, test profile (`opt-level = 1`) on the build PC; `bounds::first_open_time_of_a_store_of_live_orders`). When the rules
behind the windows change (`db::BOOK_RULES_VERSION`, the pinned templates, the listing minimum) every row is decided again, once.
