# KOB executor read API (HTTP and WebSocket)

Reference for bot and integration developers. The read API is served by `kob-executor` (roles `run` and `index`) from the
indexer's database: it answers order books, orders, fills, candles, token holdings and the indexer's own health. The node is
never contacted by a request, so public load cannot reach it. It cannot place, fill, cancel or refund anything; transactions are
built and submitted elsewhere (`crates/kob-cli`, the web app, `crates/kob-wasm`).

Everything below was written from the code of `crates/kob-executor/src/api/` (router, guard, handlers, WebSocket, rate limiter,
cache, connection limits, client address resolution), `config.rs` (`[api]`) and the response types the handlers serialise
(`indexer/reads.rs`, `indexer/market.rs`, `indexer/pairs.rs`, `indexer/status.rs`). Operator-side material (running an indexer,
sizing, runbooks) is in [ops/executor.md](ops/executor.md) (Part B, section 5 for the read API) and, for a public read server
behind Cloudflare Tunnel, [ops/chart-server.md](ops/chart-server.md). The `KOB1` payload that makes orders visible is
[spec/kob1-payload.md](spec/kob1-payload.md); order kinds and terms are [spec/order-types.md](spec/order-types.md); what a matcher
may fill is [spec/matcher.md](spec/matcher.md).

Contents: [1 Overview](#1-overview) | [2 Conventions](#2-conventions) | [3 Endpoints](#3-endpoints) |
[4 WebSocket](#4-websocket) | [5 Errors and limits](#5-errors-and-limits) | [6 The `[api]` configuration](#6-the-api-configuration) |
[7 Compatibility](#7-compatibility)

## 1. Overview

* **Base path** `/v1`. **GET only.** Any other method on a known path gets `405` from the router (the framework's default
  answer, with an empty body). `OPTIONS` is answered `204` on every path when `cors_allow_origin` is set (it is handled before
  routing), otherwise it is a `405` like any other method. An unknown path is `404` with the error envelope
  (`not_found`, "no such route").
* **JSON** (`application/json`) for every route except `GET /v1/metrics` (Prometheus text, `text/plain; version=0.0.4`) and the
  WebSocket. Errors use one envelope (section 5).
* **Plain HTTP/1.1.** The executor terminates no TLS; put a reverse proxy or CDN in front of a public listener
  ([ops/chart-server.md](ops/chart-server.md)).
* **Default listen address** `127.0.0.1:8090` (loopback). Change it with `[api] listen` or `--listen`; `--no-api` (or
  `[api] enabled = false`) turns the API off.
* **No authentication, no keys, no cookies.** Every route is public to whoever can reach the listener.
* **CORS.** No CORS header is sent unless `[api] cors_allow_origin` is set. When it is, every response (including errors and the
  `204` preflight) carries `Access-Control-Allow-Origin: <that value>`, `Access-Control-Allow-Methods: GET, OPTIONS` and
  `Access-Control-Allow-Headers: content-type`. The value is sent as configured (use `*` for a public server). The WebSocket
  upgrade checks no `Origin`.
* **No `Cache-Control`.** The API sets no `Cache-Control` header on any route (it keeps its own server-side response cache for a few
  hot reads, section 5.6). The headers it adds itself are `Content-Type`, `Retry-After` on some errors, and the CORS headers
  when configured.
* **Staleness.** Answers reflect the indexer's store, which follows the chain a few blocks behind the node (`GET /v1/health`
  says how far). The mempool is not served: an order or fill appears once its transaction is in a chain block the indexer
  applied. Every view derived from a chain position carries `confirmations` and `settled` (section 2.6).

### Routes

The list below is the router (`api/mod.rs`, `build()`), in registration order: 21 routes, plus `GET /v1/metrics` as the 22nd
only when `[api] metrics = true` (the default). "Cost" is the rate-limit weight (section 5.2): `1` light, `H` heavy
(`heavy_route_cost`, default 5). "Cached" marks the reads kept in the response cache (section 5.6).

| # | Route | Section | Cost | Cached |
|---|---|---|---|---|
| 1 | `GET /v1/health` | [3.1](#31-get-v1health) | 1 | no |
| 2 | `GET /v1/health/ready` | [3.2](#32-get-v1healthready) | 1 | no |
| 3 | `GET /v1/tokens` | [3.4](#34-get-v1tokens) | 1 | yes |
| 4 | `GET /v1/tokens/{covenant_id}/declaration` | [3.5](#35-get-v1tokenscovenant_iddeclaration) | 1 | no |
| 5 | `GET /v1/books/{token}` | [3.6](#36-get-v1bookstoken) | H | yes |
| 6 | `GET /v1/pairs` | [3.7](#37-pairs) | H | no |
| 7 | `GET /v1/pairs/{base}/{quote}/book` | [3.7](#37-pairs) | H | no |
| 8 | `GET /v1/pairs/{base}/{quote}/candles` | [3.7](#37-pairs) | H | no |
| 9 | `GET /v1/pairs/{base}/{quote}/fills` | [3.7](#37-pairs) | H | no |
| 10 | `GET /v1/orders` | [3.8](#38-get-v1orders) | 1 | no |
| 11 | `GET /v1/orders/{covenant_id}` | [3.9](#39-get-v1orderscovenant_id) | 1 | no |
| 12 | `GET /v1/orders/{covenant_id}/events` | [3.10](#310-get-v1orderscovenant_idevents) | 1 | no |
| 13 | `GET /v1/fills` | [3.11](#311-get-v1fills) | 1 | no |
| 14 | `GET /v1/strays` | [3.12](#312-get-v1strays) | H | no |
| 15 | `GET /v1/token-events` | [3.13](#313-get-v1token-events) | 1 | no |
| 16 | `GET /v1/token-utxos` | [3.14](#314-get-v1token-utxos) | H | no |
| 17 | `GET /v1/trades/{token}` | [3.15](#315-get-v1tradestoken) | H | no |
| 18 | `GET /v1/candles/{token}` | [3.16](#316-get-v1candlestoken) | H | yes |
| 19 | `GET /v1/stats/{token}` | [3.17](#317-get-v1statstoken) | H | yes |
| 20 | `GET /v1/depth/{token}` | [3.18](#318-get-v1depthtoken) | H | yes |
| 21 | `GET /v1/ws` | [4](#4-websocket) | 1 (upgrade) | no |
| 22 | `GET /v1/metrics` (only with `metrics = true`) | [3.3](#33-get-v1metrics) | 1 | no |

## 2. Conventions

### 2.1 Numbers and strings

Amounts do not fit a JavaScript number (a token supply can reach 2.9e18 base units), so **every sompi amount, price and every
token base-unit quantity is a decimal string**. Counts, scales, DAA scores, ids and timestamps are JSON numbers.

| Spelled as a decimal string | Fields (wherever they appear) |
|---|---|
| sompi | `value`, `cur_value`, `payout`, `price`, `quote`, `tip`, `budget_rate`, `reserve`, `start_price`, `end_price`, `current_price`, `quote_volume`, `quote_volume_24h`, `cum_quote`, `tip_kas`, `prefund`, `delivery_carrier`, the prices of candles/stats/depth/trades (`o`, `h`, `l`, `c`, `last`, `open_24h`, `high_24h`, `low_24h`, `best_bid`, `best_ask`, `mid`), `price_basis` |
| token base units | `amount`, `amount_left`, `filled_amount`, `initial_amount`, `min_fill`, `volume`, `volume_24h`, `cum_amount`, `amount_a`, `amount_b`, `expected_amount`, `rpt_amount`, `rearm_amount` |
| pair price fractions | `price_num`, `price_den`, and `value`/`num`/`den` of a pair candle rate |
| order state | the 64-bit fields inside `state` / `placed_state` (the contract's own field names, e.g. `minFill`, `amountLeft`, `price`) |
| WebSocket `fill` frames | `price`, `amount`, `payout` (decimal strings; `price` and `payout` may be `null`) |

| Spelled as a JSON number | Fields |
|---|---|
| scales and decimals | `scale`, `market_scale`, `base_scale`, `quote_scale`, `a_scale`, `decimals`, `quote_decimals` |
| counts | `orders`, `open_asks`, `open_bids`, `trades`, `trades_24h`, `fills`, `pair_fills`, `spread_bps`, `change_24h_bps`, ... |
| chain positions | every `*_daa` field (`genesis.daa`, `current_daa`, `expiry_daa`, `created_daa`, `daa`, `node_daa`, `cursor_daa`, `daa_score`), `block_seq`, `last_block`, `index`, `out`, `confirmations` |
| ids and times | event/fill/trade `id`, `ts` and candle `t` (chain block time, unix **milliseconds**), `deadline` (unix **seconds**), `*_unix_ms`, `*_unix` |
| small codes | order `side` (1 sells the token, 2 buys it), `tif` (0 GTC/GTD, 1 IOC, 2 FOK), order `family` (a code), `owner_kind` |

**Prices are sompi per whole token** (`scale` base units of the order's token; a pair order's price is token B base units per
whole A). `scale` is `10^decimals` of the token, at most `10^9`. Market views (trades, candles, stats, depth) report prices per
`price_basis` base units, which is the token's standard scale when its decimals are known, else the scale of its first order; they
also carry `decimals` (`null` when unknown). The decimals of a token are, in this order: its verified KCC-23 declaration, its
registry entry, the default of its token program (a launchpad's convention: KaspaRocket 3; `default_decimals` of the program
profile), the same order the web app and `kob-cli` use. `GET /v1/tokens` names the source (`decimals_source`).

Fields marked "may be null" are present with value `null`; fields marked "omitted" are absent when they do not apply (section 7
lists the omitted ones).

### 2.2 Hex ids and keys

* Responses spell every id, hash and key as **lowercase hex**: covenant ids (also called token ids and order ids), template
  hashes, transaction ids, block hashes, the extension commitment, x-only public keys. Covenant ids, template hashes, txids and
  `cursor_hash` are 32 bytes (64 hex characters).
* Path and query parameters that name one of those (`{covenant_id}`, `{token}`, `{base}`, `{quote}`, `token=`) must be **exactly
  32 bytes of hex**; otherwise the answer is `400 bad_request` ("`<name>` must be 32 bytes of hex"). Upper or lower case and an
  optional `0x` prefix are accepted; the response is always lowercase.
* **`maker`** (`/v1/orders`, `/v1/strays`) is a maker's **64-hex x-only public key** (the `maker` field of an order). The code
  checks only that the value is non-empty, even-length hex: a shorter hex string is not rejected, it simply matches no order.
  A Kaspa address (`kaspa:...`, `kaspatest:...`) is not hex and gives `400` ("maker must be hex"); convert the address to its
  32-byte key first.
* **`owner`** of `/v1/token-utxos` is more permissive: 32-byte hex (a key or a covenant id) **or** a Kaspa P2PK address
  (section 3.14).

### 2.3 Paging

* `limit` is a positive integer. Omitted: the endpoint's default. Larger than the cap: **silently clamped** to the cap. Zero,
  negative or not an integer: `400` ("limit must be a positive integer"; for `/v1/depth` the parameter is `levels`, for
  `/v1/books` and `/v1/pairs/.../book` it is `depth`). The cap is `[api] max_page_size` (default 200) everywhere except
  `/v1/candles` and `/v1/pairs/.../candles`, whose `limit` is capped at 1500.
* Pages come back as `{"items": [...], "next_cursor": ...}` where `next_cursor` is `null` on the last page. A cursor is **opaque
  to clients**; the shapes below are descriptive.
* Unknown query parameters are ignored.

| Endpoint | Order | Continue with | `next_cursor` is |
|---|---|---|---|
| `/v1/orders` | newest first | `cursor=` | `<genesis block seq>:<covenant id>` of the last item |
| `/v1/orders/{id}/events` | oldest first | `after=` (an event id) | id of the last event |
| `/v1/fills` | newest first | `before=` (an event id) | id of the last fill |
| `/v1/trades/{token}` | newest first | `before=` (a trade id) | id of the last trade |
| `/v1/pairs/{base}/{quote}/fills` | newest first | `before=` (a fill id) | id of the last fill |
| `/v1/token-utxos` | oldest first | `cursor=` | `<created block seq>:<txid>:<index>` of the last item |
| `/v1/strays`, `/v1/token-events` | newest / oldest first | none: `{"items": [...]}` only | (no cursor; `limit` is the only control) |

`before=` and `after=` parse as 64-bit integers (`400` "before must be an integer").

### 2.4 Order status

`status` of an order (`OrderView.status`, and the `status=` filter of `/v1/orders`):

| Value | Meaning |
|---|---|
| `open` | Live, nothing filled yet. |
| `partial` | Live, something filled (a repeating entry that sold out stays `partial` with `amount_left` `"0"` until refunded or cancelled). |
| `filled` | Ended by a fill. |
| `cancelled` | Ended by a cancel. |
| `refunded` | Ended by a refund (soft expiry, idle, or a keeper). |
| `killed` | IOC / FOK: the unfilled rest was returned (by the same transaction or by a keeper). |
| `closed` | Ended by something the indexer could not classify, or closed by gap reconciliation. |
| `active` | **Filter only**: `open` or `partial`. Never a stored status. |

A filter value outside this list is `400` ("status must be one of open, partial, filled, cancelled, refunded, killed, closed,
active"). An order view whose state row does not exist reports `status: "unknown"`; that value is not filterable. `expired: true`
marks a live order past `expiry_daa` whose refund is pending.

### 2.5 Candle intervals

`interval` is one of `1m`, `5m`, `1h`, `1d` (default `5m`); anything else is `400` ("interval must be one of 1m, 5m, 1h, 1d").
A bucket starts at `floor(ts / interval) * interval` in UTC milliseconds. `from` and `to` are unix **milliseconds**, the window
is `[from, to)`, and `to` must be greater than `from`.

### 2.6 Confirmations and settlement

Orders, events, fills, trades, holdings and pair fills carry `confirmations` (the node's DAA score minus the DAA of the chain
block, never negative) and `settled` (`confirmations >= settle_depth_daa`; `settle_depth_daa` is a top-level executor setting,
default 100, reported by `/v1/health`). Both are `null` / `false` while the node's DAA score is unknown. Consumers choose their
tier: tentative (any accepted event, may still reorg) or settled. Facilitators should default to settled
([ops/executor.md](ops/executor.md), section 5).

## 3. Endpoints

All examples use `$API` for the base URL (for example `http://127.0.0.1:8090`) and `$TOKEN` / `$ORDER` for a 64-hex covenant id.
Hex values in the example responses are shown as `<hex32>` or `<hex>`; ids, DAA scores and amounts are illustrative.
"Abridged" means fields are left out for brevity; the field tables list what the code serialises.

### 3.1 `GET /v1/health`

The indexer's status. Always `200` while the process serves (including `ok: false`, `state: "gap"`); poll it, and use
`/v1/health/ready` for load balancers. No parameters. The counters are read on the light lane (never behind heavy reads).

```
curl -s $API/v1/health
```

```json
{
  "state": "following",
  "network": "testnet-10",
  "node_version": "<string>",
  "cursor_hash": "<hex32>",
  "cursor_daa": 123456789,
  "node_daa": 123456791,
  "last_error": null,
  "gap_reason": null,
  "last_poll_unix_ms": 1790000000000,
  "last_progress_unix_ms": 1790000000000,
  "reorgs_total": 3,
  "reverted_blocks_total": 5,
  "blocks_applied_total": 812345,
  "relevant_txs_total": 4210,
  "orders_total": 295,
  "records_next_n": 4210,
  "lag_blue": 0,
  "batch_window_blue": 600,
  "last_batch_blocks": 1,
  "last_batch_txs": 2,
  "last_batch_bytes": 1800,
  "last_fetch_ms": 12,
  "wire_bytes_total": 91000000,
  "txs_fetched_total": 100000,
  "vspc_timeouts_total": 0,
  "consecutive_failures": 0,
  "prefetch_windows": 0, "prefetch_in_flight": 0, "prefetch_bytes": 0, "prefetch_bytes_peak": 0,
  "prefetch_window_blocks": 0, "prefetch_discards_total": 0, "prefetch_oversize_total": 0,
  "lag_tolerance_daa": 300,
  "untrusted_windows_total": 0, "untrusted_blocks_total": 0, "confirmed_blocks_total": 0, "lies_total": 0,
  "book_complete": true,
  "lean": false, "lean_orders_pruned_total": 0, "lean_rows_pruned_total": 0,
  "ok": true,
  "caught_up": true,
  "within_lag_tolerance": true,
  "lag_daa": 2,
  "lag_seconds": 0,
  "bytes_per_tx": 910,
  "alarms": [],
  "settle_depth_daa": 100,
  "min_order_value_sompi": "100000000",
  "counters": { "orders_total": 295, "orders_listed": 240, "orders_by_status": { "open": 12, "filled": 200, "cancelled": 83 },
                "rejects_total": 7, "fills_total": 270, "last_block_seq": 812345, "last_block_daa": 123456789 },
  "version": "0.1.0",
  "commit": null,
  "payload_version": 4
}
```

The example is a store that follows a single node, with values made up for illustration (a store with several nodes also has
`nodes`; a catching-up or partial one has the omitted keys listed below).

Every key the handler returns:

**The build that answers**

| Key | Type | Meaning |
|---|---|---|
| `version` | string | The crate version of the running build, the release tag without the `v` (`0.1.0` is the first release; a build of an earlier commit says `0.0.0`; [ops/release.md](ops/release.md), section 7). |
| `commit` | string or `null` | The git commit the build was made from; `null` when the build did not set `KOB_GIT_COMMIT` (a plain `cargo build`). |
| `payload_version` | number | The `KOB1` payload version this build reads and writes ([spec/kob1-payload.md](spec/kob1-payload.md)); currently 4. |

**Follower state**

| Key | Type | Meaning |
|---|---|---|
| `state` | string | `starting` (opening the database, connecting to the node), `catching_up` (applying batches, more chain remains before the node's sink), `following` (at the node's sink and polling), `node_unavailable` (the node is unreachable, not synced or in a transitional IBD state; the cursor does not advance), `gap` (the stored chain cannot be continued from the node: downtime beyond the node's pruning, or a different network; operator action needed, [ops/executor.md](ops/executor.md) Part B 7.4), `stopped`. |
| `network` | string | The configured network (for example `testnet-10`). |
| `node_version` | string or `null` | The node's server version; `null` until the first successful contact. |
| `cursor_hash` | hex or `null` | The last applied chain block. |
| `cursor_daa` | number | DAA score of the cursor. |
| `node_daa` | number or `null` | The node's virtual DAA score at the last successful poll; `null` until known. |
| `last_error` | string or `null` | The last follower error (set while `node_unavailable`, cleared on progress). |
| `gap_reason` | string or `null` | Why the chain cannot be continued (set with `state = gap`). |
| `last_poll_unix_ms` | number | Wall time of the last poll. |
| `last_progress_unix_ms` | number | Last time the cursor advanced or the node confirmed the store is at its sink (stamped on every successful poll, including "nothing new"). |
| `ok` | bool | `state` is `following` or `catching_up` **and** `alarms` is empty. Note a catching-up store can be `ok`. |
| `caught_up` | bool | `state = following` and `node_daa` known. The matcher and keepers plan only against a caught-up store (or within the tolerance below). |
| `within_lag_tolerance` | bool | `caught_up`, or `following` / `catching_up` with a lag of at most `lag_tolerance_daa` (when that is above 0). What the matcher, keepers and maintenance gate on. |
| `lag_tolerance_daa` | number | The tolerance in DAA: `max_lag_secs` (default 30) times `daa_per_second` (10) = 300; 0 means strict (only `following`). |
| `lag_daa` | number or `null` | `node_daa` minus `cursor_daa` (not below 0); `null` while `node_daa` is unknown. |
| `lag_seconds` | number or `null` | `lag_daa / daa_per_second` (integer division; `daa_per_second` default 10); `null` with `lag_daa`. |
| `lag_blue` | number or `null` | Blue score between the cursor and the node's sink at the last request; `null` before it is measured. |
| `alarms` | array | One entry `{"lag_hours_at_least": h, "lag_seconds": s}` per configured threshold the lag has reached. Thresholds are the top-level `lag_alarm_hours` (default `[1, 6, 12, 24]`). Empty while `lag_seconds` is unknown. |
| `settle_depth_daa` | number | The depth (in DAA) at which a view counts as `settled`; default 100. |
| `min_order_value_sompi` | string | The smallest order this executor lists, in sompi as a decimal string (`[rules] min_order_value_sompi` of its config; default `"100000000"`, 1 KAS): the order's amount at its quote, a bid's escrow. A smaller order is indexed (status, cancel) but never listed (`unlisted_reason: "order_value_below_minimum"`), so it is in no book and never fills. The web app warns below it. |

**Counters and totals** (all numbers)

| Key | Meaning |
|---|---|
| `reorgs_total`, `reverted_blocks_total`, `blocks_applied_total` | Chain reorganisations, reverted blocks and applied chain blocks since the process started. |
| `relevant_txs_total` | Transactions KOB cares about that were applied (they go to the record log). |
| `orders_total` | Orders indexed (the same figure as `counters.orders_total`). |
| `records_next_n` | Position of the next record-log entry; not advancing while `relevant_txs_total` grows means a record-log write failure. |
| `batch_window_blue` | The current batch window in blue score; `null` until measured. |
| `last_batch_blocks`, `last_batch_txs`, `last_batch_bytes`, `last_fetch_ms` | The last batch: chain blocks, accepted transactions, bytes on the wire, fetch time. |
| `wire_bytes_total`, `txs_fetched_total` | Bytes and accepted transactions fetched since the start. |
| `bytes_per_tx` | `wire_bytes_total / txs_fetched_total` (integer); `null` until a transaction was fetched. |
| `vspc_timeouts_total` | Chain-fetch requests that timed out (each split the batch). |
| `consecutive_failures` | Failed polls in a row; 0 after any progress. |
| `prefetch_windows`, `prefetch_in_flight`, `prefetch_bytes`, `prefetch_bytes_peak`, `prefetch_window_blocks`, `prefetch_discards_total`, `prefetch_oversize_total` | Parallel fetch: windows queued ahead of the cursor and in flight, their bytes now and at most, the current window in chain blocks, plans dropped because the chain moved, windows refused as oversize. |
| `untrusted_windows_total`, `untrusted_blocks_total`, `confirmed_blocks_total`, `lies_total` | Several nodes: windows and chain blocks fetched from a node other than the primary, how many were compared with the primary's copy before applying, and contradictions found. |
| `lean`, `lean_orders_pruned_total`, `lean_rows_pruned_total` | Lean storage: whether it is on, and the closed orders / rows it deleted since the start. |
| `counters` | Object or `null` (`null` when the counter read failed; health never turns into an error for it): `orders_total`, `orders_listed`, `orders_by_status` (object status to count, statuses with 0 omitted), `rejects_total` (KOB1 payloads that produced no order), `fills_total`, `last_block_seq`, `last_block_daa`. |

**History and gaps**

| Key | Type | Meaning |
|---|---|---|
| `book_complete` | bool | The store holds every order the node's history still shows. `false` during a fast start (the history is being indexed into a second store; see `backfill`) and for good in a store that began after the oldest history (`start = "sink"` or a block hash, or a rebase past unread chain). |
| `history_from_daa` | number, **omitted** | Where this store's history begins when it does not begin at the node's pruning point; `0` when the start block's score was not known yet. Orders created before it are not in this store. Absent for a store that read the node's whole retained history. |
| `catch_up` | object, **omitted** | While `catching_up`: `{"rate_daa_per_sec": number, "eta_seconds": number or null}`, the pace over the last minute (chain moves at about 10 DAA per second) and the seconds until caught up (`null` while the lag does not shrink). Absent once following, and until two samples a second apart exist. |
| `backfill` | object, **omitted** | The fast start's full store, absent once the book is complete: `state` (follower state of the full store), `cursor_daa`, `lag_daa` / `lag_seconds` (or `null`), `rate_daa_per_sec` (or `null`), `eta_seconds` (or `null`), `tip_since_unix` (when the fast store started following), `gaps` (omitted when empty). |
| `gaps` | array, **omitted** when empty | Stretches of chain the store skipped on purpose since the start (the node pruned history before it was read), oldest first: `from_daa`, `to_daa`, `to_hash`, `at_unix`, `orders_closed` (outputs closed with reason `gap`), `orders_adopted`. Orders created inside a skipped stretch and still open are not in the store until their maker imports them ([ops/executor.md](ops/executor.md) 7.4, 7.5). |

**Nodes**

| Key | Type | Meaning |
|---|---|---|
| `nodes` | array, **omitted** with a single node | Per node: `url`, `node` (the host a resolver entry uses now; omitted otherwise), `replaced`, `disagreements`, `tip_differs`, `role` (`primary` or `secondary`), `state` (`ok`, `backoff`, `dropped`, `idle`), `fetch`, `submit`, `connections`, `in_flight`, `windows_ok`, `windows_failed`, `blocks`, `txs`, `bytes`, `bytes_per_sec` (or `null`), `last_fetch_ms`, `timeouts`, `out_of_sync`, `lies`, `dropped_reason` (or `null`), `backoff_secs`, `last_error` (or `null`), `submits_ok`, `submits_failed`. Node host names are visible here and in `/v1/metrics`. |

Fields that can be `null`: `node_version`, `cursor_hash`, `node_daa`, `last_error`, `gap_reason`, `lag_daa`, `lag_seconds`,
`lag_blue`, `batch_window_blue`, `bytes_per_tx`, `commit`, `counters`. Fields that are omitted when not applicable:
`history_from_daa`, `catch_up`, `backfill`, `gaps`, `nodes`.

### 3.2 `GET /v1/health/ready`

A load-balancer probe. No parameters. Body: `{"ready": bool, "reason": string, "state": string, "lag_seconds": number or null}`.

| Status | `reason` | When |
|---|---|---|
| `200` | `ok` | `state` is `following` or `catching_up`, `lag_seconds` is known, and the lag is below the smallest `lag_alarm_hours` threshold (default 1 hour; with no thresholds configured the lag check is skipped). |
| `503` | `node position unknown` | `following` / `catching_up` but `node_daa` is not known yet. |
| `503` | `indexer lag above the first alarm threshold` | `following` / `catching_up` with `lag_seconds` at or above the smallest threshold times 3600. |
| `503` | `indexer is not following the chain` | any other state (`starting`, `node_unavailable`, `gap`, `stopped`). |

`200` is not "caught up": a store that is catching up with under an hour of lag is `ready`. For "caught up" read
`caught_up` / `state = following` of `/v1/health`.

### 3.3 `GET /v1/metrics`

Registered only when `[api] metrics = true` (the default); with `metrics = false` the path is an ordinary `404 not_found`. The
follower's state in the Prometheus text format (`# HELP` / `# TYPE` / sample lines, `kob_indexer_*` names such as
`kob_indexer_caught_up`, `kob_indexer_lag_daa`, `kob_indexer_book_complete`), with the same numbers as `/v1/health`. With several
nodes it adds per-node series labelled by node URL and role. The route is public like the rest of `/v1/`; set `metrics = false`
on a listener the public reaches ([ops/chart-server.md](ops/chart-server.md), section 2).

### 3.4 `GET /v1/tokens`

Tokens the indexer lists: the registry/allowlist entries plus, when a program allowlist is loaded, every token of a supported
program that an order placement named (at most 5000 such rows). No parameters. Cached. Response `{"tokens": [TokenView]}`.

```
curl -s $API/v1/tokens
```

```json
{ "tokens": [ {
    "ticker": "KRON", "covenant_id": "<hex32>", "template_hash": "<hex32>", "extension_commitment": "<hex32>",
    "family": "kcc20", "decimals": 3, "decimals_source": "registry", "scale": 1000, "market_scale": null,
    "registry": { "source": "kob-tokens.json", "sha256": "<hex32>" },
    "genesis_verified": null, "warnings": ["genesis_unverified"], "powers": [], "template_id": "KCC20Ref_8x8",
    "open_asks": 4, "open_bids": 3, "declaration": true
} ] }
```

| Field | Type | Meaning |
|---|---|---|
| `ticker` | string | The registry ticker; empty string for a token without a registry entry (identify tokens by covenant id plus template hash; tickers collide). |
| `covenant_id` | hex | The token's covenant id (its identity). |
| `template_hash`, `template_id` | hex / string, may be null | The token program's hash and registry id. |
| `extension_commitment` | hex, may be null | KCC-20 extension commitment. Without an entry: the one commitment the token's order placements named; `null` when they named several or none. |
| `family` | `"kcc20"`, `"kron"` or null | The token's program family. |
| `decimals` | number or null | The token's decimals: its verified KCC-23 declaration, else its registry entry, else the default of its token program (KaspaRocket 3); `null` when none of them says. |
| `decimals_source` | `"declaration"`, `"registry"`, `"program"`, **omitted** | Where `decimals` come from; omitted with `decimals: null`. |
| `scale` | number or null | The standard scale `10^decimals` (at most `10^9`) of a registry entry that has `decimals`; `null` otherwise (a token whose decimals are derived has no enforced scale: its orders keep the market's `market_scale`). |
| `market_scale` | number or null | Only for a token without a registry `scale`: the scale of its earliest order, which stays the market's scale; `null` otherwise. |
| `registry` | object or null | The registry document the facts come from: `source` (path or `inline`) and `sha256` of its exact bytes. The operator's copy, not chain data. |
| `genesis_verified` | bool or null | The registry's fact: every genesis output was checked against the pinned program; `null` when it does not say. |
| `warnings` | array of string | Derived from facts: `genesis_unverified` (unless `genesis_verified` is true), `live_mint_authority` (a genesis record lists a live mint authority; supply can still grow). Show them. |
| `powers` | array of string | Capabilities of the program that reach balances or supply (`freeze`, `seize`, `blacklist`, `mint-authority`, ...). |
| `open_asks`, `open_bids` | number | Listed live asks / bids (the orders the KAS book lists, section 3.6). |
| `declaration` | `true`, **omitted** | Present only when the indexer holds a verified KCC-23 declaration for the token. |

KOB makes no per-token judgement: there is no standing or "official" flag.

### 3.5 `GET /v1/tokens/{covenant_id}/declaration`

The token's KCC-23 launch proof (`kob-kcc23-proof/1`): the preparation and genesis transactions in the fields their ids commit
to. The indexer keeps a proof only after verifying it, and a client verifies it again: an indexer can withhold a declaration,
never forge one. The stored JSON is served as it is (`application/json`). One primary-key lookup on the light lane. Field detail
and capture rules: [ops/executor.md](ops/executor.md), section 5 (table row for this route) and
[spec/token-metadata.md](spec/token-metadata.md).

| Status | When |
|---|---|
| `200` | the indexer holds a declaration for the id |
| `400` | `covenant_id` is not 32 bytes of hex ("covenant_id must be 32 bytes of hex") |
| `404` | none held ("no KCC-23 declaration for this covenant id") |

```
curl -s $API/v1/tokens/$TOKEN/declaration
```

```json
{ "format": "kob-kcc23-proof/1", "covenantId": "<hex32>", "preparation": { "...": "..." }, "genesis": { "...": "..." } }
```

### 3.6 `GET /v1/books/{token}`

The KAS book of a token (pair orders are in no KAS book). Asks ascending, bids descending by **price per base unit**
(`price / scale`). Only orders a conforming matcher may fill now are listed: live (`open`/`partial`), listed, with their exact
custody for asks, not flagged `possibly_frozen`, and active at the node's DAA score (not before `activeFrom`, not past the soft
expiry, not idle 90 days, an IOC/FOK not past its kill time, a day order not past its deadline). Others stay visible in
`/v1/orders`. Cached.

| Param | Type | Default | Meaning |
|---|---|---|---|
| `depth` | integer | 20 | Rows per side, at most `max_page_size`. |
| `aggregate` | `true`/`1`/`false`/`0` | `true` | `true`: price levels; `false`: individual orders. Anything else: `400` "aggregate must be true or false". |

```
curl -s "$API/v1/books/$TOKEN?depth=5"
```

```json
{ "token": "<hex32>", "aggregated": true, "node_daa": 1000,
  "asks": [ { "price": "250000", "amount": "11000", "amount_estimated": false, "orders": 2, "scale": 1000 } ],
  "bids": [ { "price": "240000", "amount": "50000", "amount_estimated": true, "orders": 1, "scale": 1000 } ] }
```

Aggregated level: `price` (sompi per whole token), `amount` (base units summed in 128 bits; a bid's amount is its buying power, an
upper bound), `amount_estimated` (true when any order at the level has no exact remaining amount), `orders` (number), `scale`
(number). Levels group by `(price, scale)`; the listed orders of a token with `decimals` all share its scale, but a token
without `decimals` may have several, and then equal `price / scale` can appear as two rows. `node_daa` is a number or `null`.

With `aggregate=false`, each of `asks` / `bids` is an array of orders: `covenant_id`, `contract`, `maker` (hex or null), `price`
(the limit; an auction's start), `quote` (the price at the node's DAA score), `auction` (object or null: `kind` `decay`, `rise`,
`stop` or `entry` (pair kinds `pair_*`), `origin_daa`, `start_price`, `end_price`, `current_price`, `elapsed_daa`, `complete`),
`tip`, `min_fill`, `scale`, `amount_left`, `amount_estimated`, `status`, `cur_value`, `expiry_daa`, `expired`, `deadline`
(unix seconds or null), `deadline_passed`, `genesis_daa`, `confirmations`, `settled`.

### 3.7 Pairs

A pair `base/quote` (two token covenant ids) is **oriented**: its pair orders are those whose base token A is `base` and quote
token B is `quote`; an order of B/A is in the B/A views and never inverted into these. `base == quote` is `400`
("base and quote must be different tokens"). Details and the pricing rules (resting liquidity only, rounding): 
[ops/executor.md](ops/executor.md), section 5.4. All four routes are heavy and uncached.

**`GET /v1/pairs?token=`** returns a JSON **array** (not an object) of pairs with at least one listed live pair order, sorted by
(base, quote). `token` (hex32, optional) keeps pairs naming it as base or quote.

```json
[ { "base": "<hex32>", "quote": "<hex32>", "direct_asks": 1, "direct_bids": 0, "entry_asks": 0, "entry_bids": 1, "conditionals": 2 } ]
```

**`GET /v1/pairs/{base}/{quote}/book?depth=`** (`depth` default 20, at most `max_page_size`; an unknown pair is an empty book).

```json
{ "base": "<hex32>", "quote": "<hex32>", "daa_score": 123,
  "asks": [ { "source": "direct", "price_num": "1", "price_den": "1", "amount": "10000", "orders": 1 } ],
  "bids": [ { "source": "route",  "price_num": "2601", "price_den": "1999", "amount": "3074", "orders": 2 } ] }
```

`source` is `direct` (a `KobPair`), `entry` (a `KobIfdPair` resting at its limit) or `route` (an implied quote of one transaction
through the two KAS books; indicative). The price of a level is `price_num / price_den` quote base units per base base unit;
`amount` is base-token base units; `orders` is a number. `asks` are ways to buy base with quote, ascending; `bids` ways to sell,
descending. `daa_score` (number) is the DAA score the quotes are evaluated at (the node's, else the cursor's).

**`GET /v1/pairs/{base}/{quote}/candles?interval=&from=&to=&limit=`** takes the parameters of `/v1/candles/{token}`
(section 3.16: `interval` default `5m`, `limit` default 500, at most 1500). The candles are **derived from the two KAS series**,
never from pair fills.

```json
{ "base": "<hex32>", "quote": "<hex32>", "interval": "1h", "price_basis": "1000", "quote_price_basis": "1000",
  "decimals": 3, "quote_decimals": 3, "price_source": "kas_books",
  "items": [ { "t": 1790000000000,
               "o": { "value": "2153", "num": "28000", "den": "13" }, "h": { "...": "..." }, "l": { "...": "..." }, "c": { "...": "..." },
               "a_traded": true, "b_traded": false, "pair_volume_a": "4000", "pair_volume_b": "4000", "pair_fills": 1 } ] }
```

A rate is B base units per `price_basis` base units of A: `num / den` the exact reduced fraction, `value` its floor.
`a_traded` / `b_traded` say which side traded in KAS in the bucket (the other side carries its last close forward). `h` and `l`
are the bounds the two series allow, not observed trades. `pair_volume_*` and `pair_fills` count pair-order fills of the bucket.

**`GET /v1/pairs/{base}/{quote}/fills?limit=&before=`** (`limit` default 50, at most `max_page_size`; `before` a fill id):

```json
{ "base": "<hex32>", "quote": "<hex32>",
  "volume_24h": { "amount_a": "4000", "amount_b": "4000", "fills": 1 },
  "items": [ { "id": 7, "txid": "<hex32>", "ts": 1790000000000, "daa": 123, "order": "<hex32>", "contract": "KobPair", "side": "ask",
               "amount_a": "4000", "amount_b": "4000", "price": "1000", "price_num": "1", "price_den": "1", "a_scale": 1000,
               "tip_kas": "40000", "counterparty": "route", "price_source": "none", "confirmations": 12, "settled": false } ],
  "next_cursor": null }
```

`side` is `ask` (the order sold A) or `bid`. `counterparty` is `route`, `netting` or `inventory`. `price_source` is always `none`:
a pair fill is volume only and never sets a price. `volume_24h` covers the 24 hours ending at the newest chain block time the
indexer has recorded. `amount_b`, `price`, `price_num`, `price_den`, `tip_kas` may be null.

### 3.8 `GET /v1/orders`

Orders, **newest first** (by genesis block, then covenant id), keyset paged. Every filter is optional; with none, the call lists
every order, 50 per page by default. A pair order is listed under both of its tokens. Orders under a template this build does
not pin are never listed.

| Param | Type | Default | Meaning |
|---|---|---|---|
| `maker` | hex | none | The maker's 64-hex x-only key (section 2.2). `400` "maker must be hex" if it is not hex. |
| `token` | hex32 | none | Token covenant id (also matches a pair order's quote token). |
| `status` | enum | none | Section 2.4 (including the `active` alias). `400` otherwise. |
| `limit` | integer | 50 | Page size, at most `max_page_size`. |
| `cursor` | string | none | The previous `next_cursor`. `400` "invalid cursor" if malformed. |

A `maker=` list adds `custody` and `strays` to its live orders (the single-order lookups' fields), so a wallet can show and
cancel without a second call per order.

```
curl -s "$API/v1/orders?maker=$MAKER&status=active&limit=20"
curl -s "$API/v1/orders?cursor=812345:$ORDER"
```

```json
{ "items": [ { "covenant_id": "<hex32>", "contract": "KobAsk", "status": "partial", "...": "(OrderView, section 3.9)" } ],
  "next_cursor": "812340:<hex32>" }
```

**What an unfiltered call costs now.** The unfiltered list reads one page in index order: the index `orders_genesis_page` on
`(genesis_block, covenant_id)` is the whole sort order, so the read stops at the page's end however many orders share a genesis
block, and its cost grows with the page asked for, not with the store ([ops/executor.md](ops/executor.md), section 5.5). Before
commit 728019c4 the list sorted every order of the newest block first, and on a store of 770,000 orders where most shared one
block a plain `GET /v1/orders` took more than the 10 s request timeout and answered `504 timeout`; that is fixed. A `504` is still
possible on any route when a read outlasts `request_timeout_ms`. The combinations that still read more than a page are
`maker` + `token`, and a `status` that many orders have combined with a `maker` or `token` that has none of them (the read walks
that maker's or token's orders to find the page); a status few orders have (`active`, `open`, `partial`) is read from the live
set instead. Prefer `status=active` plus a `maker` or `token` for "my live orders".

### 3.9 `GET /v1/orders/{covenant_id}`

One order. `404 not_found` ("unknown covenant id") when the id is unknown **or** its template is not one this build pins
([spec/template-retirement.md](spec/template-retirement.md)); `400` when the id is not 32 bytes of hex. Light lane. The
single-order view adds `children`, `custody` and `strays`.

```
curl -s $API/v1/orders/$ORDER
```

```json
{ "covenant_id": "<hex32>", "contract": "KobAsk", "template_hash": "<hex32>", "family": 1, "side": 1, "maker": "<hex32>",
  "token": "<hex32>", "token_template_hash": "<hex32>", "extension_commitment": "<hex32>",
  "scale": 1000, "min_fill": "1000", "price": "250000", "tip": "0", "tif": 0, "expiry_daa": 99999999, "active_from": 0,
  "in_book": true, "budget_rate": null, "reserve": null, "initial_amount": "10000",
  "listed": true, "unlisted_reason": null, "origin": "<string>", "parent": null,
  "genesis": { "txid": "<hex32>", "out": 0, "block_seq": 2, "daa": 800, "confirmations": 200, "settled": true },
  "status": "partial", "filled_amount": "6000", "amount_left": "4000", "amount_estimated": false,
  "current": { "txid": "<hex32>", "index": 0, "value": "123456789012" }, "state_known": true,
  "state": { "kind": "KobAsk", "state": { "maker": "<hex32>", "scale": "1000", "price": "250000", "amountLeft": "4000", "...": "..." } },
  "current_daa": 990, "deadline": null, "deadline_passed": false, "refund_due_daa": 90000990, "kill_daa": null,
  "auction": null, "quote": "250000", "repeat": null,
  "custody": { "expected_amount": "4000", "utxo": { "txid": "<hex32>", "index": 1, "token": "<hex32>", "owner": "<hex32>",
               "amount": "4000", "value": "100000000", "role": "custody", "created_daa": 990, "spent": false, "spent_txid": null,
               "confirmations": 10, "settled": false }, "ok": true },
  "strays": [], "last_block": 9, "last_daa": 990, "confirmations": 10, "settled": false, "expired": false,
  "possibly_frozen": false, "possibly_frozen_basis": null, "children": ["<hex32>"], "warnings": ["genesis_unverified"] }
```

| Field | Type | Meaning |
|---|---|---|
| `covenant_id`, `contract`, `template_hash` | hex / string / hex | Identity and kind (`KobAsk`, `KobBid`, `KobCondAsk`, `KobIfdBid`, `KobPair`, ... and the `...Kron` kinds); see [spec/order-types.md](spec/order-types.md). |
| `family` | number | Family code of the order's token program (1 kcc20, 2 kron). |
| `side` | number | 1 sells the token, 2 buys it. |
| `maker`, `token`, `token_template_hash`, `extension_commitment` | hex, may be null | Maker key, token and its program, extension commitment. For a pair order `token` is the base token A. |
| `scale` | number or null | Base units per whole token (price denominator). |
| `min_fill`, `price`, `tip`, `budget_rate`, `reserve`, `initial_amount` | string, may be null | Terms. `price` is `null` for the conditional kinds and pair orders; `initial_amount` is `null` for a bid (its quantity is its escrow). |
| `tif`, `expiry_daa`, `active_from` | number, may be null | Time in force (0 GTC/GTD, 1 IOC, 2 FOK), soft expiry, activation DAA. |
| `in_book` | bool | Eligible for the KAS book (false for pair orders). |
| `listed`, `unlisted_reason` | bool, string or null | Whether the indexer lists the order and why not (`non_standard_scale`, `order_value_below_minimum`, `token_not_allowlisted`, `bad_state:...`, among others). Unlisted orders are indexed (status, cancel) but are in no book. |
| `origin`, `parent` | string, hex or null | How the order came to be and, for an if-done exit, its parent entry. |
| `genesis` | object | `txid`, `out`, `block_seq`, `daa`, `confirmations`, `settled`. |
| `status` | string | Section 2.4. |
| `filled_amount`, `amount_left`, `amount_estimated` | string, string or null, bool | Base units filled and left (a bid's `amount_left` is its buying power, an upper bound; `amount_estimated` flags that). |
| `current`, `state_known` | object or null, bool | The current outpoint `{txid, index, value}` and whether its state is proven. |
| `state` | object or null | The decoded current state `{"kind", "state"}` with the contract's field names, 64-bit fields as strings. |
| `placed_state` | object, **omitted** | For an order without a current state: its placement state, so its type and terms can still be shown. Never the basis of a spend. |
| `current_daa` | number or null | DAA of the block that created the current UTXO. |
| `deadline`, `deadline_passed` | number or null, bool | Day orders: unix seconds after which conforming matchers do not fill. |
| `refund_due_daa`, `kill_daa` | number or null | From this DAA anyone may refund; for IOC/FOK the kill time. |
| `auction`, `quote` | object or null, string or null | An auction in progress (decay, rise, armed stop leg or entry) and the quote now (the auction price, else the limit; `null` for pair orders). |
| `repeat` | object or null | Repeating if-done entry (`role: "entry"`, `rpt_amount`, `rearm_amount`) or booked exit (`role: "exit"`, `parent`, `rpt_until`, `rpt_price`). |
| `custody` | object, **omitted** | Ask kinds on single lookups and in live orders of a `maker=` list: `expected_amount`, `utxo` (the one live custody UTXO or null), `ok` (exactly one live custody holds the expected amount). |
| `strays` | array, **omitted** | Live token UTXOs owned by the order id that are not its custody (never liquidity). Single lookups and live orders of a `maker=` list. |
| `last_block`, `last_daa`, `confirmations`, `settled` | number, number, number or null, bool | Latest event position of the order. |
| `expired` | bool | Live and past `expiry_daa`: a refund is pending. |
| `possibly_frozen`, `possibly_frozen_basis`, `frozen_reason` | bool, string or null, string **omitted** | The operator's own engine pre-simulation was rejected by the token program (a judgement of this operator's matcher, not chain data); such an order is not listed as liquidity. `frozen_reason` is omitted when not flagged. |
| `children` | array, **omitted** | Covenant ids of orders created by this order (if-done exits). Single lookups only. |
| `pair` | object, **omitted** | Pair orders (`KobPair`, `KobCondPair`, `KobIfdPair`): `base`, `quote`, `base_family`, `quote_family`, `base_template_hash`, `quote_template_hash`, `base_scale`, `quote_scale`, `side` (`ask`/`bid`), `price`, `price_num`, `price_den`, `stop_price` (omitted), `quote_now`, `amount_left`, `custodies` (`token`, `role` `base`/`quote`, `expected_amount`, `utxo`, `ok`), `prefund` (omitted), `delivery_carrier`. See [ops/executor.md](ops/executor.md) 5.4. |
| `warnings` | array of string | Show these with the order: `genesis_unverified` unless every token it trades is `genesis_verified`, `live_mint_authority`. |

A token UTXO object (`custody.utxo`, `strays[]`) has `txid`, `index`, `token`, `owner`, `amount`, `value` (the KAS carrier),
`role` (`custody` or `stray`), `created_daa`, `spent`, `spent_txid`, `confirmations`, `settled`, and, when the indexer proved it,
`state` (the token state JSON that kob-wasm's builders take) and `program`; `foreign: true` appears only on a stray of a token
other than the order's own.

### 3.10 `GET /v1/orders/{covenant_id}/events`

The order's lifecycle events, **oldest first**. `404` ("unknown covenant id") when the order is unknown or not pinned by this
build. Light lane.

| Param | Type | Default | Meaning |
|---|---|---|---|
| `after` | integer | none | Return events with an `id` greater than this (the last id you have seen). `400` if not an integer. |
| `limit` | integer | 100 | At most `max_page_size`. |

```
curl -s "$API/v1/orders/$ORDER/events?after=41"
```

```json
{ "items": [ { "id": 42, "covenant_id": "<hex32>", "block_seq": 3, "daa": 905, "ts": 1790000000000, "txid": "<hex32>", "tx_pos": 1,
               "kind": "fill", "token": "<hex32>", "side": 1, "amount": "2000", "price": "250000", "payout": "5000",
               "closes": false, "detail": { "k": 1 }, "confirmations": 95, "settled": false } ],
  "next_cursor": null }
```

| Field | Type | Meaning |
|---|---|---|
| `id` | number | Event id (the paging key). |
| `block_seq`, `daa`, `ts`, `txid`, `tx_pos` | number, number, number (ms), hex, number | Where it happened. |
| `kind` | string | `create`, `fill`, `kill`, `refund`, `cancel`, `amend`, `sweep`, `rearm`, `arm`, `trail`, `update`, `unknown`. |
| `token`, `side` | hex or null, number or null | The order's token and side. |
| `amount` | string or null | Base units: filled (`fill`), `amountLeft` at creation (`create`), returned (`kill`), left (`refund`, `amend`, `sweep`), merged back (`rearm`). |
| `price` | string or null | The fill's quote (an auction: at the fill's time argument), the order's limit otherwise. |
| `payout` | string or null | What the maker was paid, sompi. |
| `closes` | bool | The event ended the order. |
| `detail` | JSON or null | Event specifics; `arm`, `trail` and the `fill` of a triggered stop carry `detail.evidence` (`input`, `order`, `side`, `price`). |
| `confirmations`, `settled` | number or null, bool | Section 2.6. |

### 3.11 `GET /v1/fills`

Recent fill events (the same item shape as 3.10, `kind` always `fill`), **newest first**. `price` can be `null` (a pair-order
fill carries no KAS price).

| Param | Type | Default | Meaning |
|---|---|---|---|
| `token` | hex32 | none | Only fills of this token. |
| `side` | `1`/`ask`/`sell` or `2`/`bid`/`buy` | none | Side of the filled order: 1 an ask (sell) was filled, 2 a bid (buy). Else `400` "side must be 1\|2\|ask\|bid". |
| `before` | integer | none | Only fills with `id` below this (the previous page's last id). |
| `limit` | integer | 50 | At most `max_page_size`. |

```
curl -s "$API/v1/fills?token=$TOKEN&limit=2"
```

Response `{"items": [EventView], "next_cursor": "<id>" or null}`; pass `next_cursor` as `before`.

### 3.12 `GET /v1/strays`

Live stray token UTXOs (token UTXOs sent to an order id outside the protocol), newest first, each with the order it is stuck on.
Heavy. No cursor.

| Param | Type | Default | Meaning |
|---|---|---|---|
| `maker` | hex | none | Only strays on orders of this maker key (section 2.2). |
| `limit` | integer | 50 | At most `max_page_size`. |

Response `{"items": [...]}`. Each item has the token UTXO fields of section 3.9 plus `order_status` (the order's status or
null), `maker` (hex or null) and `lost` (true when the order has terminated, so the maker's cancel can no longer sweep it).

### 3.13 `GET /v1/token-events`

Token identities first seen in placement records, **oldest first**. There is no cursor, so this returns the oldest `limit` rows.

| Param | Type | Default | Meaning |
|---|---|---|---|
| `token` | hex32 | none | Only this token. |
| `limit` | integer | 50 | At most `max_page_size`. |

```json
{ "items": [ { "token": "<hex32>", "template_hash": "<hex32>", "extension_commitment": null, "kind": "seen", "txid": "<hex32>", "daa": 800 } ] }
```

### 3.14 `GET /v1/token-utxos`

Token UTXOs (holdings) of tracked tokens with **proven** states, by owner and/or token. A token output is a P2SH output whose script
hash commits to the whole state, so the indexer reads the state from the creating transaction and keeps it only if its P2SH under
the pinned token program equals the output's script public key. Every token of a pinned program is tracked, listed or not.
Heavy. The limits of what is indexed (genesis outputs of other issuers, tokens sent before the indexer started, spends inside a
gap) and why a consumer must still check a UTXO against the node before spending it are in
[ops/executor.md](ops/executor.md), section 5.1.

| Param | Type | Default | Meaning |
|---|---|---|---|
| `owner` | hex32 or address | none | The state's 32-byte owner: an x-only public key or a covenant id in hex (an order id lists its custody and strays), **or** a Kaspa P2PK (Schnorr) address. A script-hash address is `400` ("owner address must be a P2PK (Schnorr) address"); anything else unparseable is `400`. The address prefix is not compared with the configured network. |
| `token` | hex32 | none | The token's covenant id. |
| `spent` | bool (`true`/`1`/`false`/`0`) | `false` | `false`: live UTXOs only; `true`: also spent ones. |
| `limit` | integer | 100 | At most `max_page_size`. |
| `cursor` | string | none | The previous `next_cursor` (`<created block seq>:<txid>:<index>`). `400` "invalid cursor" if malformed. |

At least one of `owner` and `token` is required: `400` ("owner or token is required"). The query matches the 32-byte owner
whatever its kind, so filter on `owner_kind` when spending. Items come **oldest first** (created block, txid, index).

```
curl -s "$API/v1/token-utxos?owner=$KEY&token=$TOKEN&limit=100"
```

```json
{ "items": [ { "txid": "<hex32>", "index": 1, "token": "<hex32>", "family": "kcc20", "program": "KCC20Ref_8x8",
               "template_hash": "<hex32>", "owner": "<hex32>", "owner_kind": 0, "amount": "300", "value": "1000000000",
               "role": "owned",
               "state": { "amount": "300", "owner": "<hex32>", "owner_scheme": 0, "borrow_scheme": 0,
                          "borrow_guard": "<hex32>", "extension_commitment": "<hex32>" },
               "state_hex": "<224 hex characters>",
               "created_daa": 123456, "spent": false, "spent_txid": null, "confirmations": 12, "settled": false } ],
  "next_cursor": null }
```

| Field | Type | Meaning |
|---|---|---|
| `txid`, `index` | hex, number | The outpoint. |
| `token` | hex | Token covenant id. |
| `family` | string | `kcc20` or `kron`. |
| `program`, `template_hash` | string, hex | The pinned token program template (`KCC20Ref`, `KCC20Ref_8x8`, `KronToken2433`, ...): the argument of kob-wasm's `tokenScriptPublicKey` and the builders' token requests. `template_hash` is empty if the program cannot be mapped. |
| `owner`, `owner_kind` | hex, number | The state's 32-byte owner and its kind: KCC-20 `owner_scheme` (0 key, 4 covenant id); KRON `id_type` (0 key, 1 script hash, 2 covenant id, 3 address presence). |
| `amount`, `value` | string | Token base units; the KAS carrier in sompi. |
| `role` | string | `owned` (a holder that is not a KOB order), `custody` or `stray` (owned by an order id). |
| `state` | object or `null` | The token state JSON as kob-wasm's builders take it (`null` if it cannot be decoded); KRON: `{"owner","id_type","amount","is_minter"}`. |
| `state_hex` | string | The state span in hex (112 bytes for KCC-20, 46 bytes for KRON). |
| `created_daa`, `spent`, `spent_txid`, `confirmations`, `settled` | number, bool, hex or null, number or null, bool | Position and spend status; `spent_txid` is set only for spends the indexer saw. |

### 3.15 `GET /v1/trades/{token}`

Trades of a token in KAS, **newest first**: all the fills of one transaction on one token form one trade. Only KAS-book fills
make prices (a pair fill never enters trades). Heavy.

| Param | Type | Default | Meaning |
|---|---|---|---|
| `before` | integer | none | Only trades with `id` below this. |
| `limit` | integer | 50 | At most `max_page_size`. |

```
curl -s "$API/v1/trades/$TOKEN?limit=2"
```

```json
{ "token": "<hex32>", "price_basis": "1000", "decimals": 3,
  "items": [ { "id": 41, "txid": "<hex32>", "ts": 1790000000000, "daa": 905, "price": "250000", "amount": "2000",
               "quote": "500000", "side": "buy", "fills": 1, "confirmations": 95, "settled": false } ],
  "next_cursor": "41" }
```

`amount` is the ask side's filled base units (the bid side's when no ask filled, for example a wallet taking a resting bid
directly); `quote` is the same fills' sompi at their quotes, rounded down per fill. The **resting** side is the side whose orders
are older (smaller genesis DAA, a tie goes to the asks): its volume-weighted price is the trade `price` (sompi per `price_basis`
base units), and `side` names the **aggressor**: `buy` when the asks rested, else `sell`. `id` is the smallest fill id of the
trade. `fills` is the number of fills in the trade (number). `next_cursor` may be present on a page that looks complete when the
read hit its row bound; continue with `before`.

### 3.16 `GET /v1/candles/{token}`

OHLCV candles in KAS, **ascending**, empty buckets omitted (carry the close forward yourself). Heavy; cached.

| Param | Type | Default | Meaning |
|---|---|---|---|
| `interval` | `1m`, `5m`, `1h`, `1d` | `5m` | Section 2.5. |
| `from`, `to` | integer (unix ms) | see below | The window `[from, to)`. `to` must be greater than `from` (`400` "to must be after from"); a non-integer is `400`. |
| `limit` | integer | 500 | Candles kept (the newest `limit` of the window), at most **1500**. |

Without `to`, the window ends with the bucket of the token's newest KAS trade; without `from`, it starts `limit` intervals before
its end.

```
curl -s "$API/v1/candles/$TOKEN?interval=1h&limit=24"
```

```json
{ "token": "<hex32>", "interval": "1h", "price_basis": "1000", "decimals": 3,
  "items": [ { "t": 1790000000000, "o": "250000", "h": "260000", "l": "240000", "c": "240000",
               "volume": "400", "quote_volume": "99200", "trades": 3 } ] }
```

`t` is the bucket start in unix milliseconds (number); `o h l c` are sompi per `price_basis` base units; `volume` is base units
and `quote_volume` sompi (strings); `trades` is a number. Every fill of the window is read (no row cap), so the cost grows with
the window's fills; the web app loads a chart once and then asks only for the bars since its newest one (`from=`).

### 3.17 `GET /v1/stats/{token}`

24 hour statistics and the top of the book. No parameters. Heavy; cached. The 24 h window ends at the newest chain block time the
indexer recorded (not the wall clock), so a stalled follower shows stale numbers rather than an empty day.

```json
{ "token": "<hex32>", "price_basis": "1000", "decimals": 3, "ts": 1790000000000,
  "last": "250000", "last_ts": 1790000000000, "last_side": "buy",
  "open_24h": "240000", "high_24h": "260000", "low_24h": "240000", "change_24h_bps": 416,
  "volume_24h": "4000", "quote_volume_24h": "990000", "trades_24h": 3, "window_truncated": false,
  "best_bid": "240000", "best_ask": "250000", "mid": "245000", "spread_bps": 408, "open_asks": 4, "open_bids": 3 }
```

`last`, `last_ts`, `last_side`, `open_24h`, `high_24h`, `low_24h`, `change_24h_bps`, `best_bid`, `best_ask`, `mid`,
`spread_bps` and `ts` are `null` when they do not exist yet. `volume_24h` (base units), `quote_volume_24h` (sompi) and prices are
strings; `trades_24h`, `open_asks`, `open_bids`, `change_24h_bps` and `spread_bps` (basis points) are numbers.
`window_truncated` is true when the day held more than 20,000 fill rows and the figures cover the newest ones. `best_bid`,
`best_ask`, `mid` and the open counts come from the listed book of section 3.6.

### 3.18 `GET /v1/depth/{token}`

Cumulative depth of the listed book, levels merged by price per `price_basis`, best first. Heavy; cached.

| Param | Type | Default | Meaning |
|---|---|---|---|
| `levels` | integer | 50 | Levels per side, at most `max_page_size`. |

```json
{ "token": "<hex32>", "price_basis": "1000", "decimals": 3, "ts": 1790000000000, "daa": 1000,
  "bids": [ { "price": "240000", "amount": "50000", "orders": 1, "cum_amount": "50000", "cum_quote": "12000000", "estimated": true } ],
  "asks": [ { "price": "250000", "amount": "11000", "orders": 2, "cum_amount": "11000", "cum_quote": "2750000", "estimated": false } ] }
```

`estimated` marks levels holding a bid, whose amounts are buying-power upper bounds. `cum_quote` is sompi (the cumulative amounts
at their level prices, rounded down per level). `ts` (number or null) is the newest event time; `daa` (number or null) is the
node's DAA score.

## 4. WebSocket

**URL** `ws://<host>/v1/ws` (`wss://` behind a TLS-terminating proxy). The route is an ordinary `GET` with the upgrade headers;
the upgrade request passes the same guard as REST (client address resolution, rate limit at cost 1, CORS headers; no `Origin`
check). The concurrency cap and request timeout bound only the handshake, not the session. Messages are JSON **text** frames in
both directions. Frames carry notices; amounts and prices inside them are strings.

### 4.1 Client messages

| Message | Effect |
|---|---|
| `{"op":"subscribe","channels":["fills","book:<token>"]}` | Adds channels. Reply `subscribed`. |
| `{"op":"unsubscribe","channels":["fills"]}` | Removes channels (names that are invalid or not subscribed are ignored). Reply `unsubscribed`. |
| `{"op":"ping"}` | Reply `{"type":"pong"}`. Counts as activity for the idle timer. |

Channel names (ids are 64 hex characters, case-insensitive on input, lowercase in frames):

| Channel | Frames | Meaning |
|---|---|---|
| `health` | `cursor`, `health` | Indexer progress (below). |
| `reorg` | `reorg` | A batch reverted blocks. |
| `fills` | `fill` | Every fill. |
| `fills:<token>` | `fill` | Fills of one token. |
| `book:<token>` | `book` | The token's books may have changed. A pair order's change is a `book` notice for both its tokens. |
| `order:<covenant id>` | `order` | The order changed (created, filled, closed, reverted). |

Anything else is an error string in the `subscribed` reply (`unknown channel \`x\``, `invalid channel \`book:zz\`: id must be 64
hex characters`). The cap is `max_ws_subscriptions` (default 64) distinct channels per connection: past it, the reply's `errors`
holds "subscription limit of 64 reached" and **the rest of that request's names are not processed**; re-subscribing a channel
already held is fine at the cap.

### 4.2 Server frames

Control replies have no `channel`; feed frames have `{"channel","type","data"}`.

| Frame | Shape |
|---|---|
| `subscribed` | `{"type":"subscribed","data":{"channels":["fills","book:<token>"],"errors":[]}}`. `channels` is the **whole** subscription set now (sorted: `health`, `reorg`, `fills...`, `book:...`, `order:...`), `errors` the strings for names that failed. |
| `unsubscribed` | `{"type":"unsubscribed","data":{"channels":[...]}}` (the whole set now). |
| `pong` | `{"type":"pong"}` |
| `error` | `{"type":"error","data":{"message":"invalid json"}}`; other messages: `unknown op`, `binary frames are not supported`. No code field. The connection stays open. |
| `resync` | `{"type":"resync","data":{"missed_events":12}}`. The client fell behind the server's broadcast buffer (1,024 batches) and missed events. |
| `fill` (channels `fills`, `fills:<token>`) | `{"channel":"fills","type":"fill","data":{"order":"<hex32>","token":"<hex32>","side":1,"price":"250000","amount":"2000","payout":"5000","txid":"<hex32>","block":"<hex32>","daa":995}}`. `price`, `amount`, `payout` are decimal strings; `price` and `payout` are `null` when there is none (a pair order or a conditional kind); `token` may be null; `side` (1 an ask was filled, 2 a bid) and `daa` are numbers. One frame per fill. |
| `book` | `{"channel":"book:<token>","type":"book","data":{"token":"<hex32>","cursor_daa":995}}` |
| `order` | `{"channel":"order:<id>","type":"order","data":{"covenant_id":"<hex32>","cursor_daa":995}}` |
| `reorg` | `{"channel":"reorg","type":"reorg","data":{"reverted_blocks":1,"added_blocks":2,"cursor_daa":995}}`, only for batches with `reverted_blocks > 0`. |
| `cursor` (channel `health`) | `{"channel":"health","type":"cursor","data":{"cursor_hash":"<hex32>","cursor_daa":995,"added_blocks":2,"reverted_blocks":0}}`, one per published batch. |
| `health` (channel `health`) | `{"channel":"health","type":"health","data":{"state":"following","cursor_daa":995,"node_daa":997,"lag_daa":2}}`, periodically (below). `node_daa` and `lag_daa` may be null. |

**`book` and `order` frames are notices**: they carry no book or order content. Refetch the REST resource
(`/v1/books/{token}`, `/v1/orders/{id}`). Batches that change nothing KOB cares about (no order, fill or revert) publish no event,
so `cursor` frames are not a per-block heartbeat; use the periodic `health` frame for liveness.

### 4.3 Limits, idle close and caps

| Rule | Value (config key) | Behaviour |
|---|---|---|
| Message size | 4,096 bytes per message and per frame | The server accepts no larger message or frame (the WebSocket layer enforces both limits; the close code for an oversized frame is the library's, not set by KOB). Keep subscribe lists short (a channel name with an id is about 70 bytes). |
| Client message rate | `ws_client_msgs_per_sec`, default 20 | Every inbound message (text, binary, ping, pong) counts in a one-second window; over it the server closes with code **1008** "message rate exceeded". |
| Idle | `ws_idle_timeout_ms`, default 60,000 | A session that sent no **application** message (a subscription change or `{"op":"ping"}`) for that long is closed with code **1000** "idle". Protocol Ping/Pong frames do not count, nor do binary frames. The check runs on the server's tick, so the close comes between 60 and 75 s of silence at the default. Send `{"op":"ping"}` at least every 30 s to be safe. |
| Server tick | min(15 s, idle timeout / 4) | Each tick the server sends a protocol Ping frame, and the `health` frame to sessions subscribed to `health`. Answer protocol Pings (most clients do it automatically). |
| Slow reader | 10 s per frame | A frame that cannot be written within 10 s ends the session (no close frame). |
| Shutdown | | The server sends a close with code **1001** "server shutting down". |
| Subscriptions | `max_ws_subscriptions`, default 64 | Per connection. |
| Connections, total | `max_ws_connections`, default 2,000 | Over it, the upgrade is refused: `503` `ws_capacity`, `Retry-After: 5`. |
| Connections, per client | `max_ws_per_ip`, default 20 | Counted per client address (IPv6 per `/64`, `rate_limit.ipv6_prefix_bits`). Over it: `429` `ws_per_ip_limit`, `Retry-After: 5`. |
| Connections, per IPv6 site | `max_ws_per_site`, default 200 (0 = off) | All `/64`s of one `/48` (`rate_limit.ipv6_site_prefix_bits`) together. Over it: `429` `ws_per_site_limit`, `Retry-After: 5`. |

The WebSocket caps use the resolved client address (section 5.4), and trusted proxies get no exemption from them. A WebSocket
is counted against these caps, not against `max_connections` (the HTTP connection cap), once upgraded.

### 4.4 Resync and reconnecting

The server keeps no history for a session and replays nothing. After a `resync` frame, after any disconnect (idle close, rate
close, restart, network), or on first connect:

1. Reconnect (back off on `429`/`503` using `Retry-After`) and resubscribe: the subscription set is per connection and starts
   empty.
2. Refetch everything you display from REST: books (`/v1/books/{token}`), the orders you track (`/v1/orders/{id}`), recent fills
   (`/v1/fills`, keeping `next_cursor`/`before` to fill any gap).
3. Subscribe **before** refetching, so a change between the fetch and the subscribe is not lost; then treat each `book` / `order`
   notice as "refetch".
4. A `reorg` frame means the chain reorganised: fills you saw may have been reverted; re-read what matters, and use `settled`
   to decide when to trust a fill.

A minimal session:

```
> {"op":"subscribe","channels":["health","fills:<token>","book:<token>","order:<covenant id>"]}
< {"type":"subscribed","data":{"channels":["health","fills:<token>","book:<token>","order:<covenant id>"],"errors":[]}}
< {"channel":"book:<token>","type":"book","data":{"token":"<hex32>","cursor_daa":995}}
< {"channel":"fills:<token>","type":"fill","data":{"order":"<hex32>","token":"<hex32>","side":1,"price":"250000","amount":"2000","payout":"5000","txid":"<hex32>","block":"<hex32>","daa":995}}
> {"op":"ping"}
< {"type":"pong"}
```

## 5. Errors and limits

### 5.1 Error envelope and codes

Every error the handlers and the guard produce is JSON:

```json
{ "error": { "code": "bad_request", "message": "limit must be a positive integer" } }
```

| HTTP | `code` | When | `Retry-After` |
|---|---|---|---|
| 400 | `bad_request` | A malformed parameter: not 32 bytes of hex, not an integer, bad `status` / `side` / `interval` / `aggregate` / `spent`, `limit` not a positive integer, invalid `cursor`, `to` not after `from`, `base == quote`, missing `owner` and `token`, an address where hex is required. The `message` says which. | no |
| 404 | `not_found` | Unknown path ("no such route", also `/v1/metrics` when `metrics = false`); unknown order ("unknown covenant id"); no declaration. | no |
| 429 | `rate_limited` | The rate limit (section 5.2). | seconds, at least 1 |
| 429 | `ws_per_ip_limit` | WebSocket upgrade over the per-client cap. | 5 |
| 429 | `ws_per_site_limit` | WebSocket upgrade over the per-IPv6-site cap. | 5 |
| 500 | `internal` | A database or serialisation error ("internal error"); logged server side. | no |
| 503 | `overloaded` | `max_concurrent_requests` requests already in flight; rejected at once, not queued. | 1 |
| 503 | `db_busy` | SQLite reported the database busy or locked ("database busy, retry shortly"). | 1 |
| 503 | `ws_capacity` | WebSocket upgrade over `max_ws_connections`. | 5 |
| 504 | `timeout` | The request ran longer than `request_timeout_ms` ("request timed out"). | no |
| 405 | (none) | A method other than GET on a known path; the framework's own empty answer, not the envelope. | no |
| 503 | (none) | A connection over a transport cap (section 5.5): a bare `HTTP/1.1 503` with `retry-after: 1`, an empty body, and the connection closed. | 1 |

Clients should branch on the HTTP status and `code`, not on `message` text. WebSocket protocol errors are `error` frames
(section 4.2) and close codes (4.3), not this envelope.

### 5.2 Rate limit

Token buckets, applied by the guard before anything else (so `OPTIONS` preflights, `404`s and WebSocket upgrades consume tokens
too; WebSocket messages after the upgrade do not, they have their own cap).

| Bucket | Refill (`rate_limit.*`) | Burst | Default |
|---|---|---|---|
| per client | `per_ip_rps` | `per_ip_burst` | 20 per second, burst 60 |
| per IPv6 site (`/48`, in addition) | `per_site_rps` | `per_site_burst` | 100 per second, burst 300 (IPv6 clients only) |
| global | `global_rps` | `global_burst` | 500 per second, burst 1000 |

* A request takes **1 token** from each bucket, or `heavy_route_cost` (default **5**; capped at the bucket's burst) when its path
  starts with `/v1/candles/`, `/v1/stats/`, `/v1/depth/`, `/v1/trades/`, `/v1/books/`, `/v1/token-utxos`, `/v1/strays` or
  `/v1/pairs` (which includes all four pair routes). Everything else, including `/v1/orders*`, `/v1/fills`, `/v1/tokens*`,
  `/v1/token-events`, `/v1/health*`, `/v1/metrics` and `/v1/ws`, costs 1. With the defaults a client can burst 12 heavy requests
  and sustain 4 per second.
* Order of charging: the client bucket, then the site bucket, then the global bucket; a request refused by an earlier bucket does
  not charge the global one, so a limited client cannot drain everybody else's share.
* A refused request gets `429` `rate_limited` and `Retry-After: N` where N is the whole seconds (rounded up, at least 1) until
  the missing tokens exist.
* `per_ip_rps = 0` disables the per-client and site buckets; `global_rps = 0` disables the global bucket.
* Buckets of idle clients expire after `bucket_ttl_secs` (600); at most `max_tracked_clients` (100,000) clients are tracked, the
  oldest are evicted first.
* The client is the resolved client address (section 5.4); IPv6 clients are keyed by their `/64` (`ipv6_prefix_bits`).

### 5.3 Request timeout and concurrency

* `max_concurrent_requests` (default 256) requests may be in flight; the next one is answered `503 overloaded` immediately.
* A request that runs longer than `request_timeout_ms` (default 10,000) is answered `504 timeout`. The wait for a free
  read-pool connection counts. The abandoned read is cancelled (not started if still queued; interrupted if running) and its
  connection returns to the pool.
* Reads run on `read_pool_size` (default 4) database connections, plus one more "light lane" connection for the reads bounded by a
  few index lookups (`/v1/health` counters, `/v1/orders/{id}` and its events, the declaration), so those never queue behind
  heavy reads. Independent of the request timeout, a single pooled read is interrupted by the database after 20 seconds
  (it would surface as `500 internal` only if `request_timeout_ms` were set above that).
* Candles and stats read every fill of their window (stats at most 20,000 rows, `window_truncated` says when cut), so a wide
  `/v1/candles` window on a busy token is the usual source of a slow request; narrow `from`/`to` or use a coarser `interval`.

### 5.4 Client address, proxies

The client is the socket peer unless that peer is in `trusted_proxies` (CIDR or bare IP). Only then is the forwarding header
(`client_ip_header`, default `x-forwarded-for`) consulted: for `x-forwarded-for` the **right-most address that is not itself a
trusted proxy** is used (everything to its left can be spoofed; all header lines are read right to left); for any other header name the value is read as a single address, and a repeated header is ignored. A missing or malformed
header falls back to the peer. IPv4-mapped
IPv6 addresses are treated as IPv4. Behind a proxy or CDN without `trusted_proxies`, every user shares one client's limits.
A non-loopback `listen` with no `trusted_proxies` logs a warning at start. An invalid `trusted_proxies` entry stops the start
with a configuration error. Example for Cloudflare Tunnel: [ops/chart-server.md](ops/chart-server.md), section 3.

### 5.5 Connection limits (below the guard)

The transport bounds connections that never complete a request:

| Rule | Config (default) |
|---|---|
| Request head must arrive in time (also the keep-alive idle limit between requests); slow-header connections are closed without an answer | `header_timeout_ms` (10,000) |
| A response write that makes no progress closes the connection; 0 = off | `write_timeout_ms` (30,000) |
| Open HTTP connections | `max_connections` (4,096) |
| Open HTTP connections per peer address (IPv6 per `/64`); 0 = no cap | `max_connections_per_ip` (64) |
| Open HTTP connections per IPv6 site (`/48`); 0 = no cap | `max_connections_per_site` (512) |

Trusted proxies are exempt from the per-address and per-site HTTP caps (every client behind them shares their address). A
connection over a cap gets the bare `503` of section 5.1 and is closed. These caps see the socket peer, not the forwarded client.

### 5.6 Response cache

`/v1/tokens`, `/v1/books/{token}`, `/v1/candles/{token}`, `/v1/stats/{token}` and `/v1/depth/{token}` are kept in a response cache
per **data generation** of the store (the generation moves after every committed batch, revert, operator import and
possibly-frozen flag). The key is the request's parameters plus, for the reads that depend on the clock (books, stats, depth),
the node's DAA score and the wall-clock second, so a cached body is byte for byte what the request would have computed.
Concurrent requests of one key wait for one read; a failed read is not kept. At most 1,024 bodies and 32 MiB per generation are
held. This is invisible to clients except as lower latency: it adds no header and no staleness beyond the store's own.

## 6. The `[api]` configuration

The `[api]` table of the executor's configuration file (`kob-executor.toml`; `kob-executor index --config ...`), with the nested
`[api.rate_limit]` table. All keys are optional; an unknown key in either table is refused at start. `--listen` and `--no-api`
override `listen` and `enabled`. Defaults are those of `config.rs`.

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Serve the read API. |
| `listen` | `127.0.0.1:8090` | Socket address to bind. |
| `trusted_proxies` | `[]` | Peers (CIDR or bare IP) allowed to set the forwarded-client header (your reverse proxy or CDN). |
| `client_ip_header` | `"x-forwarded-for"` | Header carrying the client address behind a trusted proxy (any other name is read as a single address). |
| `max_concurrent_requests` | `256` | Requests in flight; over it `503 overloaded`. |
| `request_timeout_ms` | `10000` | Per-request deadline; over it `504 timeout`. |
| `header_timeout_ms` | `10000` | Time for a request head to arrive (and keep-alive idle limit). |
| `write_timeout_ms` | `30000` | A write with no progress for this long closes the connection; 0 = off. |
| `max_connections` | `4096` | Open HTTP connections (an upgraded WebSocket counts against `max_ws_connections` instead). |
| `max_connections_per_ip` | `64` | Open HTTP connections per peer (IPv6 per `ipv6_prefix_bits`; trusted proxies exempt); 0 = no cap. |
| `max_connections_per_site` | `512` | Open HTTP connections per IPv6 site (`ipv6_site_prefix_bits`); 0 = no cap. |
| `max_ws_connections` | `2000` | Open WebSocket connections. |
| `max_ws_per_ip` | `20` | WebSocket connections per client address (IPv6 per `ipv6_prefix_bits`). |
| `max_ws_per_site` | `200` | WebSocket connections per IPv6 site; 0 = no cap. |
| `ws_idle_timeout_ms` | `60000` | Close a session with no application message (subscription change or `{"op":"ping"}`) for this long. |
| `ws_client_msgs_per_sec` | `20` | Inbound messages per second before the client is disconnected. |
| `max_ws_subscriptions` | `64` | Channels per WebSocket connection. |
| `read_pool_size` | `4` | Read connections (plus one light-lane connection). |
| `max_page_size` | `200` | Cap of `limit` / `depth` / `levels` on list responses. |
| `cors_allow_origin` | unset | Value of `Access-Control-Allow-Origin`; unset = no CORS headers and `OPTIONS` is `405`. |
| `metrics` | `true` | Serve `GET /v1/metrics`; `false` removes the route (`404`). |
| `rate_limit.per_ip_rps` | `20.0` | Sustained requests per second per client; 0 disables rate limiting per client (and site). |
| `rate_limit.per_ip_burst` | `60` | Burst per client. |
| `rate_limit.global_rps` | `500.0` | Sustained requests per second across all clients; 0 = unlimited. |
| `rate_limit.global_burst` | `1000` | Global burst. |
| `rate_limit.bucket_ttl_secs` | `600` | Idle time after which a client bucket is forgotten. |
| `rate_limit.max_tracked_clients` | `100000` | Hard cap on tracked clients. |
| `rate_limit.ipv6_prefix_bits` | `64` | IPv6 clients are limited and capped per this many leading bits (48 groups a site). |
| `rate_limit.ipv6_site_prefix_bits` | `48` | IPv6 clients also share a bucket and caps per this wider prefix; 0 disables the site bucket and site caps. |
| `rate_limit.per_site_rps` | `100.0` | Sustained requests per second of one IPv6 site. |
| `rate_limit.per_site_burst` | `300` | Burst of one IPv6 site. |
| `rate_limit.heavy_route_cost` | `5` | Tokens a heavy read costs (section 5.2); a light request costs 1. |

Related top-level keys that shape what the API reports: `settle_depth_daa` (100), `daa_per_second` (10), `lag_alarm_hours`
(`[1, 6, 12, 24]`), `max_lag_secs` (30).

A public read server behind a tunnel (see [ops/chart-server.md](ops/chart-server.md)):

```toml
[api]
listen = "127.0.0.1:8090"
trusted_proxies = ["127.0.0.1", "::1"]
client_ip_header = "cf-connecting-ip"
cors_allow_origin = "*"
metrics = false
```

## 7. Compatibility

What the code and the docs support, and no more:

* **Status.** The project is pre-audit and its README says "Interfaces, encodings and artifact ids can still change". No
  deprecation policy, version negotiation or release-by-release API changelog is documented. The path prefix is `/v1`.
* **Conventions the code names as its contract.** `indexer/reads.rs` labels these "stable API contract": ids, hashes and keys are
  lowercase hex; every sompi amount and token base-unit quantity is a decimal string; prices and tips are sompi per whole token;
  scales, counts, DAA scores, block sequence numbers and indices are JSON numbers; views derived from a chain position carry
  `confirmations` and `settled`. This document relies on exactly these.
* **Changes have happened.** The WebSocket `fill` frame's `price`, `amount` and `payout` moved from JSON numbers to decimal
  strings on testnet-10 ([ops/executor.md](ops/executor.md), section 5.0 marks it a breaking change), and the v2.4 receipt
  endpoint is retired (`/v1/receipts` is a `404`). Treat any release as able to change a shape, check `version`, `commit` and
  `payload_version` of `/v1/health` against the release you integrate with ([ops/release.md](ops/release.md), section 7), and
  pin your client to a tested build.
* **Defensive client habits** (advice, not a promise): ignore fields you do not know; treat an absent optional field as `null`
  (these are omitted when they do not apply: health `history_from_daa`, `catch_up`, `backfill`, `gaps`, `nodes`; token
  `declaration`; order `placed_state`, `custody`, `strays`, `frozen_reason`, `children`, `pair`; pair `stop_price`, `prefund`;
  token UTXO `state`, `program`, `foreign`); parse amounts as arbitrary-precision integers; never assume a list is complete
  without following `next_cursor`.
* **Numbers that depend on the operator.** Page caps, rate limits, timeouts and connection caps are operator configuration
  (section 6); a public server may set stricter values than the defaults here.
