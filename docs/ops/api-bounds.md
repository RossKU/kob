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
| `/v1/books/{token}` | H | S, P (`depth`, `aggregate`) | per side the best 1,000 fillable orders with a fixed quote (`BOOK_ORDERS_MAX`), 200 fillable auctions and 200 opened since the last commit (`BOOK_MOVING_MAX`); orders that cannot be filled now are not read | `bounds::the_book_reads_do_not_grow_with_listed_curve_asks` (12,000 / 24,000 curve asks; aggregate: 197,877 / 197,877; orders: 201,917 / 201,917); `bounds::orders_that_cannot_be_filled_now_take_no_place_in_the_book` (honest book alone, then with 12,000 not-yet-active, 12,000 between TWAP slices and 12,000 expired asks at a better price: 5,975 / 5,976) |
| `/v1/depth/{token}` | H | S, P | one book read | same (depth: 197,886 / 197,886; with the junk: 5,984 / 5,985) |
| `/v1/stats/{token}` | H | S | four side reads as the book's; at most 20,000 fills of the 24 h window (`MAX_FILL_ROWS`) | same (stats: 394,463 / 394,463; with the junk: 12,105 / 12,000); `bounds::the_market_reads_do_not_grow_with_fills` (25,000 and 50,000 fills: 952,573 / 952,466) |
| `/v1/stats?since=` | H | G, P (minute) | the activity buckets of the range | `bounds::the_market_reads_do_not_grow_with_fills` (activity: 211 / 211) |
| `/v1/pairs?token=` | H | S | the oldest 20,000 fillable `pair_live` rows (`PAIR_SCAN_MAX`; with `token`, that many as base and as quote) | `bounds::the_pair_reads_do_not_grow_with_pair_orders` (rows the writer set: see the wall-time table below; an older build's unset rows, read row by row: 6,380,049 / 6,380,040) |
| `/v1/pairs/{a}/{b}/book` | H | S, P | 20,001 fillable `pair_live` rows of the pair, at most 2,000 resting, and 200 opened since the last commit; the KAS route of each token as the book's rows | same (unset rows: 6,220,179 / 6,220,179; 20,001 real fillable orders: 62,183, `bounds::worst_case_pair_reads_over_real_pair_orders`); `bounds::the_book_reads_do_not_grow_with_listed_curve_asks` (pair book, KAS route: 605 / 605) |
| `/v1/pairs/{a}/{b}/candles` | H | P | the two tokens' candle rows of the window (`limit` at most 1,500) and the pair fills of the shown buckets, at most 200,000 (`PAIR_VOLUME_ROWS_MAX`) | `bounds::the_pair_candles_stop_at_their_row_cap` (210,000 / 420,000 fills in the shown buckets: 2,001,469 / 2,001,331) |
| `/v1/pairs/{a}/{b}/fills` | H | U | a page (`limit` + 1) and at most 20,001 fills of the 24 h window | `bounds::the_pair_reads_do_not_grow_with_pair_orders` (pair fills: 201,253 / 201,210) |
| `/v1/orders?maker=&token=&status=` | H | S, P | a page of at most `max_page_size` + 1 orders, each with its custody (2 rows), at most 101 strays and a count of at most 10,000 index entries | `bounds::the_order_view_does_not_grow_with_children_strays_or_spent_custody` (orders of a maker: 138,575 / 138,575) |
| `/v1/orders/{id}` | L | U | the order, at most 101 children and 101 live strays, two counts of at most 10,000 index entries each (`VIEW_COUNT_MAX`, index only), the custody and the bought-custody (2 rows each, live partial indexes) | same (order view: 220,489 / 220,489; bought custody: 91 / 91) |
| `/v1/orders/{id}/events` | L | U | `limit` + 1 events (at most 201) | same (order events: 3,398 / 3,398) |
| `/v1/fills` | H | S, P | `limit` + 1 fills | `bounds::the_market_reads_do_not_grow_with_fills` (fills: 1,545 / 1,545) |
| `/v1/trades/{token}` | H | U | `limit` trades of fills (the over-fetch of their transactions) | same (trades: 58,020 / 58,020) |
| `/v1/candles/{token}` | H | P | the candle rows of the window, at most `limit` (1,500) | same (candles: 120 / 121) |
| `/v1/quotes/{token}` | H | P | the `book_quotes` rows of the window, at most `limit` | same (quotes: 58 / 58) |
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
| `/v1/books/{token}?aggregate=false&depth=200` | 1,200 curve asks of 1,000 levels at the best price | 47 ms | 201,917 | 8 per token and second: `depth` is computed at 20, 50, 100 or 200 (`api::rest::DEPTH_STEPS`) and cut, times `aggregate` |
| `/v1/books/{token}` (levels, depth 200) | the same | 34 ms | 197,877 | (above) |
| `/v1/depth/{token}?levels=200` | the same | 72 ms | 197,886 | 4 per token and second (the same steps) |
| `/v1/stats/{token}` | the same; and 25,000 / 50,000 fills of the day | 71 ms; 53 ms | 394,463; 952,990 | 1 per token and second |
| `/v1/quotes` sampling (once a minute) | the same | 5 ms | 194,509 | none (the writer's) |
| `/v1/pairs?token=` | 20,001 fillable `KobPair` asks, real states and custodies | 25 ms | 780,122 | 1 per token and second |
| `/v1/pairs/{a}/{b}/book?depth=` | the same | 1.9 ms | 62,183 | 4 per pair and second (the steps) |
| `/v1/pairs/{a}/{b}/candles` | 420,000 pair fills in the shown buckets (`PAIR_VOLUME_ROWS_MAX` 200,000 read) | **226 ms** | 2,001,331 | one per `interval`, `from`, `to`, `limit`: not bounded |
| `/v1/pairs/{a}/{b}/fills` | 24 h of fills (20,001 read) | 19 ms | 201,210 | one per request (uncached) |
| `/v1/orders?maker=` | a maker's page at its bound | 2.6 ms | 138,575 | one per request parameters and second |
| `/v1/orders/{id}` | 10,000 children and strays | 3.8 ms | 220,489 | one per request (light lane, uncached) |
| `/v1/orders/{id}/events` | 201 events | 0.3 ms | 3,398 | one per request |
| `/v1/fills`, `/v1/trades/{token}` | 50,000 fills | 0.3 ms; 2.4 ms | 1,545; 58,020 | one per parameters (and second) |
| `/v1/candles/{token}`, `/v1/quotes/{token}` | 1,500 rows | 0.1 ms | 121; 58 | one per parameters: not bounded, each tens of microseconds |
| `/v1/stats?since=` | the activity buckets | 0.1 ms | 211 | one per minute value |
| `/v1/strays?maker=` | 10,000 live strays of the maker | 2.8 ms | 150,072 | one per request |
| `/v1/token-utxos?owner=&token=&spent=` | the owner's 10,000 holdings of other tokens | 1.4 ms | 110,149 | one per request |
| `/v1/creator-locks?due=`, `/v1/token-events`, the logo lists, the declaration | as in the table above | under 0.2 ms | under 1,400 | |

What changed to get there (R5C2-6): a book read decodes at most `depth` orders of each range ranked by key (the rest of the
range can only tie the worst level kept), the aggregated levels and the statistics stop at the first row worse than what they
need, and the pair reads take each order from its `pair_live` row as the writer decided it (side, source, scale, the amount one
fill can take, its quote as a function of the DAA score) instead of decoding its state and reading its custody.

**Still above 100 ms: `/v1/pairs/{a}/{b}/candles`**, 226 ms at its row cap, with keys a client chooses freely (`from`, `to`,
`limit`). The 200,000 pair-fill rows of the shown buckets cost their maker one transaction each. Not changed in this round.

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

**The rule of the places (R5A-1, R5C2-1):** a place among the best 1,000 of a side is taken only by an order that offers a taker
at least the listing minimum (`min_order_value_sompi`, 1 KAS by default) of real value right now. Real value is what one fill can
take from the order's UTXOs now under the covenant's own arithmetic (`indexer::takeable`), at its limit: an ask's exact custody; a
bid's buying power; a buy-first if-done entry what its KAS escrow pays after the carriers of the fill; a sell-first entry what its
KAS funds of the prefund and exit carrier of a fill it outlives; a TWAP or DCA order one slice; a curve its amount at its current
level. An order that can fund no fill takes no place at all (`ok` 0); one worth less than the minimum (for example one its own
maker filled down to one base unit) is **dust**: it stays listed, executors fill it, and the book and depth show it from a range
of its own of at most 200 (`auction` = 2), never from the 1,000. The 24 h statistics' best bid and ask (and so mid, spread and
the 2 % depth), the sampled quotes of `/v1/quotes` and the KAS legs of a pair book's route read no dust at all, since one base
unit at any price would set them; the `/v1/tokens` counts count it (it is a fillable order), within their bound.

**What an attacker must do now to push a real order out of a book read**, per kind on the side (each listed, active, not expired,
a resting kind with an open slice):

| Kind | What each of the 1,000 must hold | Who can take it |
|---|---|---|
| `KobAsk`, `KobAskKron`, `KobCurveAsk`, `KobIfdAsk` (+ KRON) | tokens worth at least 1 KAS at its price in its exact custody (a TWAP ask: per slice); a sell-first entry also the KAS of the prefund and exit carrier of a partial fill, else it can only be taken whole | anyone, at once, below the market |
| `KobBid`, `KobBidKron` | at least 1 KAS of buying power (a DCA bid: per slice) above the carriers and reserve | anyone, at once, above the market |
| `KobIfdBid` (+ KRON) | at least 1 KAS that one fill can spend from its escrow after its delivery and exit carriers | anyone, at once, above the market |
| unarmed stops, conditionals (`KobCond*`), IOC / FOK takers | not in the books | |

That is at least 1,000 KAS of tokens (asks) or KAS (bids) really on offer, which anyone can take at once. Auctions and dust take
at most their own 200 places each. Orders not active yet, between slices, expired, past their deadline, without custody, possibly
frozen or unable to fund a fill take none, whatever their number or price.

The pair book reads the 20,001 oldest fillable pair orders. Pair orders have no KAS listing minimum (a pair order has no KAS
quote), so there is no dust class there: each must fund one fill (a `KobPair` bid or a buy-first `KobIfdPair` with its B custody,
a sell-first one with its prefund, the carriers and tip in KAS) and have something left (R5A-1, R5A-2), but it may be small; the
cost of filling the 20,001 is their carriers, about 0.16 KAS each, recoverable (accepted, R5A-2).

Rows an older build (schema 14) writes through the triggers have no window yet (`ok` NULL): they count as fillable and are
decided row by row, within the same bounds, until this build's next commit or start sets them.

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
