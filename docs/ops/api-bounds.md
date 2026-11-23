# Bounds of the public read API

Every public route of the read API (`kobd --indexer`, `docs/api-reference.md`), what one computation of it reads at most, and
the test that measures it. The tests seed the rows an attacker can add cheaply, run the route's read with N and then 2 N of them
(both past the route's bound: N is the bound plus 2,000 rows, or 5,000 fills past the 20,000 of the 24 h window), and assert that the SQLite VM steps of the read do not grow with N (within 5 % and 500 steps),
and that the plans of the statements that serve it have no table scan and no temporary B-tree over those rows. The VM steps are
counted by a progress handler over every statement the read runs (`crates/kobd/src/indexer/bounds_tests.rs`, `steps`). The
x402 routes are not part of this API.

Lanes: **L** light (one connection, one read per client at a time), **H** heavy (`read_pool_size`, 4 connections, two reads per
client), **M** memory only. Cache: **G** per data generation (the store's commits), **S** per generation and wall-clock second
(and the node's DAA score), **P** per parameters the client chooses, **U** uncached (the CDN keeps `public, max-age` answers, see
`docs/api-reference.md` 5.7). Test names are in `crates/kobd/src/indexer/bounds_tests.rs` (`bounds`), `crates/kobd/src/api/tests.rs`
(`api`) and `crates/kobd/tests/` (by file).

| Route | Lane | Cache | Worst case one computation reads | Test (VM steps measured with N / 2 N rows) |
|---|---|---|---|---|
| `/v1/health`, `/v1/health/ready`, `/v1/metrics` | M | the counters at most once a second, one light read outside any client | nothing per request; outside the concurrency cap | `api::health_answers_while_every_request_permit_is_taken`, `api::health_answers_while_the_light_lane_is_busy` |
| `/v1/tokens` | H | G | per listed token, two counts of at most 10,000 `book_live` rows (`BOOK_SCAN_MAX`); the open list at most 5,000 rows (`OPEN_TOKEN_ROWS_MAX`) | `bounds::the_book_reads_do_not_grow_with_listed_curve_asks` (tokens: 791,090 / 791,032) |
| `/v1/tokens/{id}/declaration` | L | U | one primary-key lookup and two transactions decoded | `bounds::token_events_and_a_declaration_do_not_grow_with_other_tokens` (declaration: 9 / 9) |
| `/v1/books/{token}` | H | S, P (`depth`, `aggregate`) | per side the best 10,000 `book_live` rows by limit price, of which at most 1,000 fillable orders are decoded (`BOOK_ORDERS_MAX`); `truncated` past them | `bounds::the_book_reads_do_not_grow_with_listed_curve_asks` (book aggregate: 162,410 / 162,410; book orders: 170,498 / 170,498) |
| `/v1/depth/{token}` | H | S, P | one book read | same (depth: 162,419 / 162,419) |
| `/v1/stats/{token}` | H | S | four side reads as the book's; at most 20,000 fills of the 24 h window (`MAX_FILL_ROWS`) | same (stats: 324,975 / 324,868); `bounds::the_market_reads_do_not_grow_with_fills` (stats, 25,000 and 50,000 fills: 948,761 / 948,654) |
| `/v1/stats?since=` | H | G, P (minute) | the activity buckets of the range | `bounds::the_market_reads_do_not_grow_with_fills` (activity: 211 / 211) |
| `/v1/pairs?token=` | H | S | the oldest 20,000 `pair_live` rows (`PAIR_SCAN_MAX`; with `token`, that many as base and as quote) | `bounds::the_pair_reads_do_not_grow_with_pair_orders` (pairs: 2,060,282 / 2,060,282; of a token: 2,080,429 / 2,080,429) |
| `/v1/pairs/{a}/{b}/book` | H | S, P | 20,001 `pair_live` rows of the pair, at most 2,000 resting; the KAS route of each token as the book's prefix | same (pair book: 2,100,539 / 2,100,539); `bounds::the_book_reads_do_not_grow_with_listed_curve_asks` (pair book, KAS route: 269 / 269) |
| `/v1/pairs/{a}/{b}/candles` | H | P | the two tokens' candle rows of the window (`limit` at most 1,500) and the pair fills of the shown buckets, at most 200,000 (`PAIR_VOLUME_ROWS_MAX`, a stated cap, not measured past it) | `market_row_bound::pair_candles_with_a_small_limit_are_the_tail_of_the_whole_series` (shape); cap by reading |
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
| `/v1/creator-locks?due=` | H | U | auto locks in `until` order until 200 live ones (`DUE_LOCKS_MAX`); the released ones before them cost a token creation each | by reading (R4C2-6) |
| `/v1/logo-blocklist`, `GET /v1/logo-votes` | M, its own connection | 5 s | the whole list of hidden logos or of voted tokens, once per 5 s for everyone | by reading (one row per token; each vote costs work and a signature) |
| `POST /v1/logo-reports`, `POST /v1/logo-votes` | its own connection | none | bounded by the request; votes are proved by `logo_votes` tests (`held_since_daa`, VM steps) | `logo_votes::tests` |
| `/v1/price/kas-usd` | M | the quote the indexer fetched | nothing from the store | none needed |
| `/v1/ws` | M | none | no read; per commit, the subscriptions times the fills (capped per address and in all) | `api::websocket_end_to_end` |

## What fills the request permits

The server keeps at most `max_concurrent_requests` (256) requests in flight. One client key (an IPv4 address, an IPv6 /64) keeps at
most 8 (`CLIENT_IN_FLIGHT`), the /64s of one IPv6 /48 at most 32 together (`CLIENT_SITE_IN_FLIGHT`). A request holds its place only
while it reads or is about to: a read that waits for its own client's lane permit gives up after 500 ms (`CLIENT_WAIT`) with
`503 overloaded`. So filling the 256 takes 32 IPv4 addresses or 8 IPv6 /48s, each sending at least 16 requests a second that
wait (at the per-client rate limit of 20 a second). The others then get `503 overloaded` with `Retry-After: 1`; `/v1/health`,
`/v1/health/ready` and `/v1/metrics` take no permit and keep answering. A response the cache shares is computed under the
permit of the client that starts it, taken before the computation exists, so the other viewers of a hot key never wait for that
client's queue (R4C2-3).

## Start-up work that grows with attacker rows

At each writer open, `db::backfill_stop_entry_books` re-derives every live if-done entry in a book and
`db::backfill_exit_extensions` walks the if-done exits without an extension commitment. Both are idempotent and run every open on
purpose: they repair rows an earlier build wrote, including after a downgrade and upgrade. Their cost is linear in live if-done
entries (each locks its order value) and in exits (one per fill of an entry), seconds per 10^5 rows, at start-up only, before the
API listens. They are not reachable from the API. `db::backfill_book_live` fills `book_live` and `pair_live` once (`meta.book_live`):
one pass over the live orders.
