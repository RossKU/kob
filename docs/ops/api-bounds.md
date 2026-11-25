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
| `/v1/tokens?cursor=&limit=` | H | G, P (`cursor`, `limit`) | a page: on the first, the registry's tokens; then at most `limit` (2,000) open tokens in four runs (the tokens the chain proves by market presence, `token_presence`; the other proven ones by age; the tokens placements name by covenant id; the tokens only listed orders name by age), each step one or two seeks, each view two counts of at most 1,400 fillable `book_live` rows, its market scale (one seek) and a few seeks; the page ends at its 2,000,000-step budget (`OPEN_LIST_STEPS_MAX`) and carries `next_cursor` (R9 follow-up) | `bounds::the_book_reads_do_not_grow_with_listed_curve_asks` (tokens: 33,934 / 33,934); `bounds::the_token_list_does_not_fail_with_open_tokens` (first page with 5,000 / 10,000 open tokens of 50 orders each: 731,826 / 731,778); `bounds::the_token_list_at_every_cap_answers_page_by_page` (first page 579,798, the dearest of 22 pages 880,019; round 8's build 5,871,852, past the budget); `bounds::market_presence_holds_the_first_places_of_the_token_list`; `bounds::a_proven_token_survives_an_open_list_flood` |
| `/v1/token-utxos?owner=&token=&order=amount` | H | U | two ranges of `token_holdings_live_amount` (owner, token, amount descending), `limit` + 1 entries each, and `limit` views | `bounds::an_owners_largest_units_come_first_whatever_dust_is_sent` (10,000 / 100,000 dust units sent to the owner, the first page: 15,455 / 15,449) |
| `/v1/pairs/{a}/{b}/orders?maker=` | H | S, P | one range of `orders_maker_pair_genesis` (maker, base, quote), at most 10,000 of the maker's orders of the pair a page (then a cursor), and `limit` views | `bounds::a_makers_pair_orders_are_read_alone` (30 live orders of the maker behind 50,000 / 100,000 live orders of others on the pair, every page: 11,987 / 11,919) |
| `/v1/tokens/{id}` | L | U | one token: its registry entry, or its proven programs and claims (a few seeks; an unproven one at most 1,024 of its order programs) and one view | `bounds::the_token_list_at_every_cap_answers_with_the_proven_tokens` (a token the list's cut left out) |
| `/v1/tokens/{id}/declaration` | L | U | one primary-key lookup and two transactions decoded | `bounds::token_events_and_a_declaration_do_not_grow_with_other_tokens` (declaration: 9 / 9) |
| `/v1/books/{token}` | H | S, P (`depth`, `aggregate`) | per side the best 1,000 fillable orders with a fixed quote (`BOOK_ORDERS_MAX`), 200 fillable auctions and 200 opened since the last commit (`BOOK_MOVING_MAX`); orders that cannot be filled now are not read | `bounds::the_book_reads_do_not_grow_with_listed_curve_asks` (12,000 / 24,000 curve asks; aggregate: 214,724 / 214,724; orders: 207,325 / 207,325); `bounds::worst_case_book_reads_over_1000_level_curves` (every range full of 1,000-level curves, N / 2 N: orders 249,385 / 253,635, with dust 287,448 / 291,698; the growth is the capped `dust_orders` count); `bounds::orders_that_cannot_be_filled_now_take_no_place_in_the_book` (honest book alone, then with 12,000 not-yet-active, 12,000 between TWAP slices and 12,000 expired asks at a better price: 5,975 / 5,976) |
| `/v1/depth/{token}` | H | S, P | one book read | same (depth: 214,423 / 214,423; with the junk: 5,984 / 5,985); a single book read of `levels` prices since R7C2b-3 |
| `/v1/stats/{token}` | H | S | four side reads as the book's; at most 20,000 priced fills of the 24 h window (`MAX_FILL_ROWS`, rows read through `order_events_priced_ts`) | same (stats: 404,576 / 404,576; with the junk: 12,105 / 12,000); `bounds::the_market_reads_do_not_grow_with_fills` (25,000 and 50,000 fills: 633,150 / 633,030) |
| `/v1/stats?since=` | H | G, P (minute) | the activity buckets of the range, the locked KAS from one counter row (`counters.kas_locked`, R7C2b-3), and the makers and executors: all time two counter rows (`counters`, `actors:1`, `actors:2`), a range at most 50,000 of each (`ACTORS_COUNT_MAX`, then `actors_truncated`; R8C2b-4) | `bounds::the_market_reads_do_not_grow_with_fills` (activity: 115 / 115) |
| `/v1/pairs?token=` | H | S | the oldest 20,000 fillable `pair_live` rows (`PAIR_SCAN_MAX`; with `token`, that many as base and as quote) | `bounds::the_pair_reads_do_not_grow_with_pair_orders` (rows the writer set: see the wall-time table below; an older build's unset rows, read row by row: 6,380,049 / 6,380,040) |
| `/v1/pairs/{a}/{b}/book` | H | S, P | 20,001 fillable `pair_live` rows of the pair, at most 2,000 resting, and 200 opened since the last commit; the KAS route of each token as the book's rows | same (unset rows: 6,220,179 / 6,220,179; 20,001 real fillable orders: 62,183, `bounds::worst_case_pair_reads_over_real_pair_orders`); `bounds::the_book_reads_do_not_grow_with_listed_curve_asks` (pair book, KAS route: 605 / 605) |
| `/v1/pairs/{a}/{b}/candles` | H | P | the two tokens' candle rows of the window (`limit` at most 1,500) and one `pair_volume` row per shown bucket (the writer's aggregates, R5C2-6); no pair fill | `bounds::the_pair_candles_read_one_volume_row_a_bucket` (210,000 / 420,000 fills in the shown buckets: 1,569 / 1,463) |
| `/v1/pairs/{a}/{b}/orders` | H | S, P (`cursor`, `limit`) | one keyset range of `pair_live_pair` per live state (5), each at most `limit` + 1 index entries, and `limit` order views; only the pair's own orders | `bounds::a_pair_order_list_reads_only_its_pair` (30 orders of the pair behind 20,000 / 40,000 live pair orders of either token with another: every page, 11,808 / 11,726) |
| `/v1/pairs/{a}/{b}/fills` | H | U | a page (`limit` + 1) and at most 20,001 fills of the 24 h window | `bounds::the_pair_reads_do_not_grow_with_pair_orders` (pair fills: 201,253 / 201,210) |
| `/v1/orders?maker=&token=&status=` | H | S, P | a page of at most `max_page_size` + 1 orders, each with its custody (2 rows), at most 101 strays and a count of at most 10,000 index entries; a status read in history order reads at most 10,000 orders of the history (`ORDERS_SCAN_MAX`; a short page carries the cursor, R6C2b-2); with a maker and a token, of that maker's orders of that token only (`orders_maker_token_genesis`, `orders_maker_quote_genesis`, R8C2b-1) | `bounds::the_order_view_does_not_grow_with_children_strays_or_spent_custody` (orders of a maker: 138,575 / 138,575); `bounds::a_maker_token_status_read_is_bounded` (2,000 cancelled orders of the maker on the token behind 20,000 / 40,000 sharing one filter, every page: 445,932 / 445,932) |
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
| `/v1/tokens` | 24,000 curve asks on one token | 0.8 ms | 33,934 | 1 |
| `/v1/tokens` (one page) | 2,000 proven tokens, event rows of 20,001 tokens with 16 orders each, listed orders of 20,001 more | 144 ms (counter on; round 8's build 880 ms at 5,871,852 steps, refused) | 579,798 | 1 per page and data generation |
| `/v1/orders?maker=&token=&status=` | 2,000 cancelled orders of the maker on the token behind 40,000 sharing one filter, every page of 200 | 95 ms (11 pages) | 445,932 | one per request parameters and second |
| `/v1/pairs/{a}/{b}/orders` | 30 orders of the pair behind 40,000 live pair orders of either token, every page of 7 | 4.5 ms (5 pages) | 11,726 | one per request parameters and second |
| `/v1/books/{token}?aggregate=false&depth=200` | 1,200 curve asks of 1,000 levels at the best price, 250 dust and 250 pending curves of 1,000 levels (2,400 / 500 / 500 for the 2 N run); with `include_dust` | 19 ms; 41 ms (re-measured 2026-11-24, R8: every curve shown as its sequence of smallest fills, at most `depth` steps a curve) | 253,635; 291,698 | 16 per token and second: `depth` is computed at 20, 50, 100 or 200 (`api::rest::DEPTH_STEPS`) and cut, times `aggregate`, times `include_dust` |
| `/v1/books/{token}` (levels, depth 200, `include_dust`) | the same | 58 ms | 297,362 | (above) |
| `/v1/depth/{token}?levels=200&include_dust=true` | the same | 59 ms (no re-read at 4 x the levels since R7C2b-3: when two scales round to one per-basis level the worst level is left out and the answer says `truncated`) | 297,066 | 8 per token and second (the same steps, times `include_dust`) |
| `/v1/stats/{token}` | the same; and 25,000 / 50,000 fills of the day (each read alone; one token holding both is about the sum, within the read budget) | 94 ms; 63 ms (the 2 % band reads each curve's whole sequence) | 478,596; 633,030 | 1 per token and second |
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

No public route but a `/v1/tokens` page is above 100 ms at its worst case (a page at every former cap: 144 ms with the step counter on, one computation per page and data generation for everyone; re-measured 2026-11-25, R9 follow-up; the book worst case is `bounds::worst_case_book_reads_over_1000_level_curves`, now a non-ignored test with a VM-steps assertion). A maker page of 200 live orders each with 10,000 live strays is not seeded: each order's view is measured at 3.8 ms with 10,000 strays, so such a page would take about 0.8 s, but it needs 2,000,000 stray token UTXOs of at least `COVENANT_OUTPUT_MIN` (0.08 KAS) each, about 1.6·10^5 KAS given to the maker, so it is left as it is. The pair candles take their volume from `pair_volume`: per pair and candle
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
at its floor; a rising bid at its first quote, with the buying power of its highest; an armed entry at the smaller offer of
its band's two ends). A curve is ranked and shown by its smallest valid fill (its minimum fill as KIP-9 raises it, or its whole rest
when KIP-9 leaves no partial fill: R8C2a-1) at the mean price a taker pays for it, and valued by the largest fill one transaction
takes (at most 8 levels); its depth is the sequence of its successive smallest valid fills (an ordinary curve: every remaining
level at its own price; a bundle forced by its minimum fill: one level at its mean price), each step by the matcher's rule at
the state the steps before it leave (its KIP-9 rule over its 8-level window; the numeric gate refuses a curve whose window
could meet a level past its table or below its tip, R8C2a-2), never below an earlier one, and a read expands only the levels its `depth` needs (`reads::curve_shown`, R8). A pair order shows the least amount one fill can
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
| `KobCurveAsk` | tokens worth at least 0.5 KAS in the largest fill one transaction takes (at most 8 levels), ranked at the mean price of its smallest valid fill | anyone, at once, at that mean price for the smallest fill |
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

**The property's oracle (R8C2a-1).** For every kind, what one fill can take at each sampled time is the matcher's own
candidate code (what an executor builds from), not the reader's; a curve's smallest valid fill is the smallest `n` its
`CurveLeg::fill_ok` accepts, and each shown step must be one. For the first 60 placed curves and the first 60 placed plain
KCC-20 asks of each run, the fill is also built with the protocol builder as an executor builds it and run through the script
engine against the order's contract script (a curve: every shown step at the state the steps before it leave, and one unit
less must not pass; an ask: its shown amount). The other kinds (bids, if-done entries, pair orders) are checked against the
matcher's code only. The generator draws repeating if-done entries, bid delivery modes 1 and 2, if-done activation times,
pair scales from 1 to 10^8, custody carriers at and around the covenant output minimum, rows that open after the commit
(checked from their opening), and checks that a row hidden as dust is worth less than the dust line at one end of its window
and that a curve's rank key is its first shown level. Open: the builder refuses for KIP-9's storage mass some fills the
matcher's per-output rule admits (the rule bounds each output alone; the block limit binds their sum); the property counts
them and does not fail on them.

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
(`kob_api_read_budget_refused_total{route}`). The largest worst case measured is a lean store's chart over 30,000 fills, 0.93 million steps; the dearest
`/v1/tokens` page at every former cap is 0.88 million (a page stops itself at 2 million and carries `next_cursor`, R9 follow-up). The
budget is about 5 times the first; the per-route caps stay what keeps honest answers complete.
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

**The wall bound counts waits (R8C2b-I1).** The 1.5 s of read time is real time from the popped connection, SQLite's busy
handler included (readers wait up to 30 s for a lock). A read that meets `SQLITE_BUSY` (a WAL recovery, a checkpoint at a stop) or
cold pages on an undersized host can therefore end as `read_budget`, not `db_busy`. Honest reads are about 15 times below the bound,
so it is rare; a client treats both as "retry in a second".

## Refusals an outsider can cause, and what an honest user sees (R8)

The rule: an honest request's cost does not grow with rows an outsider can write; a list says `truncated` (or carries a cursor)
instead of failing. Where a refusal is still possible, its cost and its effect are stated here.

| Where | Who can cause it, and at what cost | What an honest user sees |
|---|---|---|
| `/v1/orders?maker=&token=&status=` in history order | Removed (R8C2b-1): the read and its probe walk one index of both filters (`orders_maker_token_genesis`, `orders_maker_quote_genesis`), so other makers' orders of the token and the maker's orders of other tokens are not read. Only orders of that maker on that token lengthen it, and the scan cap (10,000) gives a short page with a cursor | A complete list, paged |
| `/v1/tokens` | Removed (R9C2b-1, R9C2b-2 and their follow-up): a page stops itself at 2,000,000 VM steps or 2,000 tokens and carries `next_cursor`, so the list never fails for its size and a full read is always possible. The first places go to the tokens the chain proves by market presence: the KAS of their fillable fixed-price bids (`token_presence`, the place rule's notion of value: not dust, not an auction). To push a real token with X KAS of such bids off the first page, an attacker needs 2,000 proven tokens with more than X KAS of fillable bids each, more than 2,000 X KAS locked in bids anyone holding those tokens can take; he gets back what is not taken (of tokens he holds himself, all of it: the cost is the KAS kept locked, and it grows with the real token's market). Tokens with no presence (a token just launched) come after them by age, then the tokens only placements name | The registry's tokens always on the first page; every other token on some page, in that order; `GET /v1/tokens/{id}` answers any token the list holds |
| The `/v1/tokens` pages, seen from clients | No client follows `next_cursor` yet: the web's market list and its token selectors read the first page | A token past the first page needs the next pages or `GET /v1/tokens/{id}` (web change, not made here) |
| `/v1/stats/{t}`, the pair-candle carry (the cross check of the newest price) | Removed (R8C2b-3): the check seeks one transaction's fills (`order_events_priced_tx (token_id, txid)`) | Answers |
| `/v1/stats?since=` (makers, executors) | Removed (R8C2b-4): all time reads two counters; a range counts at most 50,000 of each and says `actors_truncated` | Totals, "at least" past the cap |
| `/v1/candles/{t}`, pair candles without `to` | Removed (R8C2b-5): the newest fill time is one seek of the priced fills' index; pair fills and self-takes are not in it | Answers |
| `/v1/orders/{id}` on a storage fault | Not an outsider's: a fault of the store answers `503 storage` (R8C2b-I3), an unrenderable order `404` | "Retry", never "unknown" for an honest order |
| Books, depth, pair book, pair orders, order view, strays, holdings, fills, trades, quotes | No outsider path to a refusal: each has a row cap well below the budget (measured above). What their caps can leave out is in the next table | Answers |
| The wall bound under host load | Someone who loads the host's disk or CPU; not demonstrated | A rare `503 read_budget` on a hot key, for a second |
| A key refused for a second | Only by making that exact key's computation pass the budget; keys are exact, so one key's refusal never reaches another | As above, for that key |
| The web's own-order list, cut at 50 pages | Per victim: about 5·10^5 orders naming the victim's key on that token (several hundred KAS of fees). Decided (R8B-L3) | That victim's ticket on that token is refused |
| The web's pair ticket check | The pair's own orders only (`/v1/pairs/{a}/{b}/orders`): each must fund one fill (about 0.16 KAS of carriers, refundable), the cost the pair book's 20,001 rows accept | The ticket waits while the list is longer than the web's page cap |
| The CLI live-order list (200 pages, a repeated or bad cursor) | A hostile indexer, or per-victim inflation as above | A waivable `own-orders` guard |

## Lanes: the chain-proof work an outsider can cause (R9-K)

The follower proves the native lane and only those KIP-21 user lanes on which a window spends an output the store tracks
(`docs/spec/indexer-lanes.md`). Each proven lane costs two lane proofs (at the anchor and at the window's end).

- **Junk lanes.** Transactions that spend nothing tracked, each on a lane of its own, cost no proof at whatever volume:
  - before, the reviewer's figure was about 0.08 KAS per 4,100 transactions, and 2 proofs each;
  - tests: `a_junk_user_lane_flood_proves_no_lane` and `a_junk_lane_flood_costs_no_lane_proofs` (4,000 lanes applied after
    the native lane's proofs alone).
- **Chained lanes.** A chain of user-lane transactions, each spending the last one's output on a fresh lane, proves its
  first link only: nothing a user-lane transaction creates is tracked (`a_chained_user_lane_flood_proves_only_its_first_link`).
- **Lanes that each end a tracked output** (the expensive case). Each proven lane ends one tracked output (an order UTXO, an
  order's token UTXO, a holding) that the attacker made earlier on the native lane. Each output is ended once
  (`user_lanes_that_each_end_a_tracked_output_are_each_proven_once`).
  - A tracked output is a covenant output, of KIP-9 storage mass `4·C/v` grams (C = 10^12, v its value in sompi).
  - Block space is 500,000 grams a block at 10 blocks a second.
  - So 1.76 M of them (an hour of consensus's maximum of 500 new lanes a second) need:

| Value of each output | Capital (refundable, at risk while out) | Blocks fully used to make them |
|---|---|---|
| 1 KAS | about 1.76 M KAS | about 3.9 h |
| 10 KAS | about 17.6 M KAS | about 23 min |

Plus the fees of about 3.5 M transactions, and orders that rest visibly in the book until they are ended. kobd's work per
link is the two lane proofs and one record. Against a remote node (the supported setup), kobd proves about 13 such lanes a
second at 0.3 s a call and 4 at 1 s: each delays the index by 75 ms or 250 ms. Pushing it past the pruning depth (30 h)
takes about 1.4 M of them at 0.3 s, or 0.43 M at 1 s (`docs/ops/executor.md`, *Node*, *Lane load*). In the normal case
fewer lanes are proven than before: the native lane alone unless a user lane spends a tracked output.

## What a cap can leave out (R9)

Every public read with a row cap, scan cap or `LIMIT` over rows an outsider writes, and which honest content the order of its scan
could push out.

| Read | Cap and order | What could be pushed out, and why that is acceptable or how it is fixed |
|---|---|---|
| `/v1/books`, `/v1/depth`, the stats' touch and band | the best 1,000 fixed places by price, 200 auctions, 200 pending (the place rule above) | Only by better-priced orders with real value on offer, which anyone can take: the book shows them because they are the market |
| `/v1/tokens` | pages in four runs, above | Fixed (R9C2b-1, R9C2b-2, follow-up): every token is on some page; the first page's places are held by market presence |
| `/v1/pairs?token=` | the oldest 20,000 fillable pair orders (of the token, as base and as quote) | Pairs whose orders all came after 20,000 older fillable pair orders: the flood funds one fill each (about 0.16 KAS of carriers, 3,200 KAS locked in all). A pair already listed keeps its place (oldest first); a pair's own book (`/v1/pairs/{a}/{b}/book`) and orders (`/orders`) answer by its ids whatever the list holds. The answer, a bare array, says the cut in the header `x-kob-truncated: 1` (R9 follow-up; the CDN keeps it with the cached answer). Not done: ordering by live value (pair orders have no KAS value to rank by, and a value per pair is not kept) and paging (no room for a cursor in the array; a v2 would answer `{"pairs", "next_cursor"}` keyed by (base, quote) over a skip-scan of `pair_live_pair`) |
| `/v1/pairs/{a}/{b}/book` | 20,001 oldest fillable orders of the pair, 2,000 resting | Newer orders of that pair, only by orders of that pair that fund a fill each; says `orders_truncated` |
| `/v1/pairs/{a}/{b}/orders` | keyset pages over the pair's listed live rows | Nothing: every page follows the cursor. Listed orders only (R9C2b-4): an unlisted order, which no executor fills, cannot cross the book, and is in `/v1/orders?maker=` |
| `/v1/orders?maker=&token=&status=` | 10,000 of the maker's orders of the token per page (both columns in one index) | Nothing: a short page carries the cursor. Only orders of that maker on that token lengthen it (the web's 50-page cut is R8B-L3, decided) |
| `/v1/orders` other filters | history order, 10,000 scanned per page, cursor | Nothing: paged |
| `/v1/token-utxos?owner=&token=` | 10,000 of the owner's holdings and the rest of a block per page, cursor | Nothing: paged |
| `/v1/strays?maker=` | the newest 10,000 live strays of all makers, filtered by maker | A maker's older strays, behind 10,000 newer strays anyone sent (each a token output with its carrier, forfeited to the receiving order). Acceptable: each of the maker's live orders carries its strays in `/v1/orders?maker=` and its order view, and the cancel sweeps them |
| `/v1/orders/{id}` | 101 children, 101 live strays, two counts of at most 10,000 | An order's strays past 101 (anyone can send them, each with its carrier); the counts say how many there are and the cancel sweeps them all |
| `/v1/trades`, `/v1/fills`, `/v1/pairs/{a}/{b}/fills` | newest first, `limit`, `before=` | Nothing: paged |
| `/v1/stats/{t}` 24 h figures | the newest 20,000 priced fills of the day | Older fills of the day behind 20,000 newer priced fills (two keys of one trader washing, fees each): `window_truncated` says so; last, high and low stay those of the fills read |
| `/v1/stats?since=` | 50,000 actors of each kind per range | Says `actors_truncated` ("at least"); all time is exact |
| lean `/v1/candles` | the newest 30,000 fills of the window | Says `truncated`; a full store charts from aggregates |
| `/v1/creator-locks?due=` | the earliest 200 due automatic locks still holding | Later ones wait until those are released (a keeper releases them, which takes them off the list); each lock is a token launch with a declaration |
| `/v1/token-events?token=` | `limit` events of the token, oldest first | Newer events of that token behind placements that name it: the first sightings come first, which is what the route is for |
| `/v1/logo-votes`, `/v1/logo-blocklist` | one row per voted or hidden token | Votes need a proof of work, an address with history and a held token; reports are rate-limited |
| search | none | The API has no search route |

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
