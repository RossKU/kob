# KOB read API (HTTP and WebSocket)

The read API answers order books, orders, fills, trades, candles, token holdings and the indexer's own health, as JSON over
HTTP and a WebSocket feed. It is for bots, dashboards and integrations.

> **Open review.** KOB has had no external audit. Paths, field names and shapes can still change. The public server below runs
> on testnet-10.

Every field, parameter and frame, the limits and the `[api]` configuration: [api-reference.md](api-reference.md). Section
numbers cited elsewhere as "api.md 3.19" and the like are the reference's sections.

## What it is

| | |
|---|---|
| Read only | It cannot place, fill, cancel or refund anything. Transactions are built and signed elsewhere: [`kob`](../crates/kob-cli/README.md), the web app, `crates/kob-wasm`. |
| Keyless | No account, no API key, no cookies. Every route is public to whoever can reach the server. |
| Who runs it | Any operator: `kobd --indexer` (or `kobd --executor`; with `--indexer` too over a full store) serves it from its indexer database, on `127.0.0.1:8090` by default. Requests never reach the node. Setup: [ops/executor.md](ops/executor.md); a public server: [ops/chart-server.md](ops/chart-server.md). |
| Public servers | `https://kob-api.kaspa.ag` (mainnet), `https://kob-api-tn10.kaspa.ag` (testnet-10). WebSocket: `wss://kob-api.kaspa.ag/v1/ws`, `wss://kob-api-tn10.kaspa.ag/v1/ws`. |
| Freshness | The store follows the chain a few blocks behind the node; mempool transactions are not served. `GET /v1/health` says how far behind it is. |

## Conventions

| Topic | Rule |
|---|---|
| Base path | `/v1`. JSON (`application/json`) everywhere except the WebSocket and `/v1/metrics`. |
| Methods | `GET`. The one write route, `POST /v1/logo-reports`, exists only when the operator turns it on. Other methods: `405`. |
| Amounts | Every sompi amount, price and token quantity is a **decimal string** (it can exceed 2^53). Counts, scales, DAA scores, ids and times are JSON numbers. |
| Token units | Quantities are in **base units**; `scale` = 10^decimals base units make one whole token (at most 10^9). |
| Prices | Book and order prices are **sompi per whole token**. Market views (trades, candles, stats, depth) are sompi per `price_basis` base units; they also carry `decimals` (`null` when unknown). |
| Ids | Lowercase hex in responses. Path and query ids must be 32 bytes of hex (either case, optional `0x`), else `400`. |
| Times | `ts` and candle `t` are unix **milliseconds** (chain block time); `deadline` is unix **seconds**. |
| Settlement | Chain-derived views carry `confirmations` and `settled` (`confirmations >= settle_depth_daa`, default 100). Trust a fill once it is `settled`. |
| Paging | `limit` (default per route, capped at 200; candles at 1,500). Pages are `{"items": [...], "next_cursor": ...}`; pass `next_cursor` back as `cursor=` or `before=` (see the endpoint table). `null` means the last page. |
| Errors | `{"error": {"code": "...", "message": "..."}}` (a `405`, a `413` or a transport-level `503` has no body). Branch on the HTTP status and `code`, not on the message. |
| Rate limit | Token bucket per client: 20 requests/s, burst 60. Heavy routes (H below) cost 5. Over it: `429 rate_limited` with `Retry-After`. |
| CORS | Sent only when the operator sets `cors_allow_origin` (the public server sends `*`); `OPTIONS` is then `204`. |
| Caching | No `Cache-Control` header (except `/v1/logo-blocklist`, 5 s, and `/v1/price/kas-usd`, 60 s). Hot reads are cached server side per indexed batch; that adds no staleness. |

The limits are the defaults; an operator can set others. Details: [api-reference.md](api-reference.md), sections 2 and 5.

## Endpoints

H = heavy (rate-limit cost 5). `{token}`, `{covenant_id}`, `{base}` and `{quote}` are 64-hex covenant ids.

| Method and path | Purpose | Page with | Cost |
|---|---|---|---|
| `GET /v1/health` | Indexer state, lag, counters, build version | | 1 |
| `GET /v1/health/ready` | Load-balancer probe: `200` or `503` | | 1 |
| `GET /v1/tokens` | Tokens with markets: ids, decimals, warnings, open order counts | | 1 |
| `GET /v1/tokens/{covenant_id}/declaration` | The token's KCC-23 launch proof (`404` if none) | | 1 |
| `GET /v1/books/{token}?depth=&aggregate=` | KAS order book: price levels or individual orders | | H |
| `GET /v1/pairs?token=` | Token/token pairs with live pair orders (a JSON array) | | H |
| `GET /v1/pairs/{base}/{quote}/book?depth=` | Pair book: direct, entry and implied route levels | | H |
| `GET /v1/pairs/{base}/{quote}/candles?interval=&from=&to=&limit=` | Pair candles derived from the two KAS series | | H |
| `GET /v1/pairs/{base}/{quote}/fills?limit=&before=` | Pair-order fills and 24 h pair volume | `before=` | H |
| `GET /v1/orders?maker=&token=&status=&limit=&cursor=` | Orders, newest first | `cursor=` | 1 |
| `GET /v1/orders/{covenant_id}` | One order, with custody, strays and children | | 1 |
| `GET /v1/orders/{covenant_id}/events?after=&limit=` | The order's lifecycle events, oldest first | `after=` | 1 |
| `GET /v1/fills?token=&side=&limit=&before=` | Fill events, newest first | `before=` | 1 |
| `GET /v1/trades/{token}?limit=&before=` | KAS trades (fills grouped per transaction), newest first | `before=` | H |
| `GET /v1/candles/{token}?interval=&from=&to=&limit=` | OHLCV candles, ascending; `interval` `1m`, `5m` (default), `1h`, `1d` | | H |
| `GET /v1/stats?since=` | Activity totals of the store: orders placed, fills, traders, executors, fees paid to fillers, KAS locked (`since` in unix ms; all time without it) | | H |
| `GET /v1/stats/{token}` | 24 h stats and the top of the book | | H |
| `GET /v1/depth/{token}?levels=` | Cumulative depth per price level | | H |
| `GET /v1/token-utxos?owner=&token=&spent=&limit=&cursor=` | Token holdings with proven states (`owner` or `token` required) | `cursor=` | H |
| `GET /v1/strays?maker=&limit=` | Token UTXOs stuck on an order id | | H |
| `GET /v1/token-events?token=&limit=` | First sightings of token identities, oldest first | | 1 |
| `GET /v1/ws` | WebSocket feed (below) | | 1 |
| `GET /v1/metrics` | Prometheus text; the operator may turn it off (the public server answers `404`) | | 1 |
| `POST /v1/logo-reports` | Report a token logo; hides it until reviewed (only if enabled) | | 1 |
| `GET /v1/logo-blocklist` | Tokens whose logo the web app does not load (only if enabled) | | 1 |
| `GET /v1/price/kas-usd` | KAS price in USD `{usd, at, source}` for the web app's optional USD display; `at` is unix seconds; cached 60 s (only if the operator enabled `usd_price`; else `404`) | | 1 |

`status=` takes `open`, `partial`, `filled`, `cancelled`, `refunded`, `killed`, `closed`, or `active` (open or partial).
`maker=` is the maker's 64-hex x-only public key, not an address; `owner=` of `/v1/token-utxos` also takes a P2PK address.

## Examples

The responses below are from the public server, trimmed (`...`); hex ids are shortened. `TBTC` on testnet-10 has 8 decimals,
so `price_basis` and `scale` are `100000000`.

```sh
API=https://kob-api-tn10.kaspa.ag
T=$(curl -s $API/v1/tokens | jq -r '.tokens[] | select(.ticker == "TBTC") | .covenant_id')   # c064d410...
```

**The book**, three levels a side:

```sh
curl -s "$API/v1/books/$T?depth=3"
```

```json
{ "token": "c064d410...", "aggregated": true, "node_daa": 615875493,
  "asks": [ { "price": "257845044900630", "amount": "6330", "amount_estimated": false, "orders": 1, "scale": 100000000 }, ... ],
  "bids": [ { "price": "257536738010170", "amount": "416", "amount_estimated": true, "orders": 1, "scale": 100000000 }, ... ] }
```

`price` is sompi per whole token: 257845044900630 sompi = 2,578,450.4 KAS per TBTC. A bid's `amount` is what its escrow can buy,
an upper bound (`amount_estimated: true`). `aggregate=false` lists the orders instead of levels.

**24 hour stats:**

```sh
curl -s "$API/v1/stats/$T"
```

```json
{ "token": "c064d410...", "price_basis": "100000000", "decimals": 8, "ts": 1793922428060,
  "last": "257665672371919", "last_ts": 1793922423356, "last_side": "sell",
  "open_24h": "249951908643110", "high_24h": "258685009287168", "low_24h": "247095947035342", "change_24h_bps": 308,
  "volume_24h": "44442132", "quote_volume_24h": "112271556316902", "trades_24h": 6910, "window_truncated": false,
  "best_bid": "257536738010170", "best_ask": "257845044900630", "mid": "257690891455400", "spread_bps": 11,
  "open_asks": 17, "open_bids": 38, "depth_2pct_quote": "943116142984" }
```

**Candles**, hourly, the newest two:

```sh
curl -s "$API/v1/candles/$T?interval=1h&limit=2"
```

```json
{ "token": "c064d410...", "interval": "1h", "price_basis": "100000000", "decimals": 8,
  "items": [ { "t": 1793916000000, "o": "255603764055156", "h": "256641442810039", "l": "254753027578535",
               "c": "256160466248031", "volume": "2441014", "quote_volume": "6243078714119", "trades": 358 }, ... ] }
```

Empty buckets are left out; carry the close forward yourself. To keep a chart current, ask only for bars since the newest one
(`from=` its `t`).

**Recent fills**, paged with `before`:

```sh
curl -s "$API/v1/fills?token=$T&limit=1"
curl -s "$API/v1/fills?token=$T&limit=1&before=3903160"    # the next page
```

```json
{ "items": [ { "id": 3903160, "covenant_id": "6a159cfc...", "block_seq": 14487207, "daa": 615875564, "ts": 1793922436685,
               "txid": "4594e3d8...", "tx_pos": 8, "kind": "fill", "token": "c064d410...", "side": 1,
               "amount": "1939", "price": "257845044900630", "payout": "4999615421", "closes": false,
               "detail": { "entry": "settle", "input": 1, "t": 0, "verified": true }, "confirmations": 26, "settled": false } ],
  "next_cursor": "3903160" }
```

`side` 1 means an ask (a sell order) was filled; `payout` is the KAS it paid the maker (`null` on a bid fill).

**A wallet's token holdings:**

```sh
curl -s "$API/v1/token-utxos?owner=<64-hex key or kaspatest: address>&token=$T"
```

```json
{ "items": [ { "txid": "d458e526...", "index": 2, "token": "c064d410...", "family": "kcc20", "program": "KCC20Ref",
               "owner": "94ade318...", "owner_kind": 0, "amount": "1520539919", "value": "100000000", "role": "owned",
               "state": { "amount": "1520539919", "owner": "94ade318...", "owner_scheme": 0, ... },
               "spent": false, "confirmations": 380, "settled": true, ... } ],
  "next_cursor": "14487049:d458e526...:2" }
```

A holding is listed only when the indexer proved its state. Check a UTXO against the node before you spend it.

**An error:**

```sh
curl -s "$API/v1/books/zz"
```

```json
{ "error": { "code": "bad_request", "message": "token must be 32 bytes of hex" } }
```

## WebSocket

Connect to `wss://<host>/v1/ws` and send JSON text messages. The feed sends **notices**: refetch the REST resource they name.
The server replays nothing.

| You send | Reply |
|---|---|
| `{"op":"subscribe","channels":["fills:<token>","book:<token>"]}` | `subscribed`: the whole subscription set and an `errors` list |
| `{"op":"unsubscribe","channels":["fills:<token>"]}` | `unsubscribed`: the set that remains |
| `{"op":"ping"}` | `{"type":"pong"}` |

| Channel | Frame | Meaning |
|---|---|---|
| `fills`, `fills:<token>` | `fill` | One fill: order, token, side, price, amount, payout, txid, block, daa |
| `book:<token>` | `book` | The token's book may have changed: refetch `/v1/books/{token}` |
| `order:<covenant id>` | `order` | The order changed: refetch `/v1/orders/{id}` |
| `health` | `cursor`, `health` | Indexer progress per batch, and a periodic status frame |
| `reorg` | `reorg` | A batch reverted blocks; fills you saw may be gone |

A short session against the public server:

```
> {"op":"subscribe","channels":["health","fills:c064d410...","book:c064d410..."]}
< {"type":"subscribed","data":{"channels":["health","fills:c064d410...","book:c064d410..."],"errors":[]}}
< {"channel":"book:c064d410...","type":"book","data":{"token":"c064d410...","cursor_daa":615875840}}
< {"channel":"fills:c064d410...","type":"fill","data":{"order":"6a159cfc...","token":"c064d410...","side":1,
   "price":"257699051090220","amount":"2135","payout":"5501874741","txid":"...","block":"53250b52...","daa":615875856}}
> {"op":"ping"}
< {"type":"pong"}
```

- **Stay connected.** Send `{"op":"ping"}` at least every 30 s: a session silent for 60 s is closed (code 1000, "idle").
  Protocol Ping frames do not count.
- **Limits.** 64 channels per connection, 20 messages per second (over it: close 1008), 4,096 bytes per message,
  20 connections per client address.
- **Reconnect.** After a `resync` frame or any disconnect: reconnect (honour `Retry-After` on `429`/`503`), subscribe first,
  then refetch what you display from REST.

Frame shapes, close codes and caps: [api-reference.md](api-reference.md), section 4.

## Health and readiness

`GET /v1/health` is always `200` while the server runs; read its fields. `GET /v1/health/ready` is the probe for a load
balancer: `200` when following (or catching up) with lag under the first alarm threshold (1 hour by default), else `503`.

```sh
curl -s "$API/v1/health"
```

```json
{ "state": "following", "network": "testnet-10", "ok": true, "caught_up": true, "within_lag_tolerance": true,
  "cursor_daa": 615875170, "node_daa": 615875186, "lag_daa": 16, "lag_seconds": 1, "alarms": [],
  "book_complete": true, "roles": ["indexer"], "settle_depth_daa": 100, "min_order_value_sompi": "100000000",
  "counters": { "orders_total": 1244635, "fills_total": 1299005, ... },
  "version": "0.1.0", "commit": null, "payload_version": 4, ... }
```

| Field | Use it for |
|---|---|
| `state` | `starting`, `catching_up`, `following`, `node_unavailable`, `gap`, `stopped`, `storage_error` (the database cannot be written or is corrupt; `halt_reason` says why) |
| `ok` | Following or catching up, with no lag alarm |
| `caught_up` | `state` is `following` and the node's position is known |
| `lag_seconds` | How far the store is behind the node (`null` while unknown) |
| `book_complete` | `false` while history is still being indexed, or for a store that began after the oldest history |
| `min_order_value_sompi` | The smallest order this server lists; smaller orders are indexed but never in a book |
| `version`, `commit`, `payload_version`, `template_generation` | The build that answers and the order templates it serves; compare with the release you integrate with |
| `newer_release_seen` | `{version, first_seen_daa}` once placements of at least 2 makers announce a KOB release newer than this build (`null` otherwise): informational, the operator should update (`docs/ops/release.md`, 7a) |
| `halt_reason`, `api_error`, `data_dir_free_bytes`, `db_bytes` | Operator monitoring: why the store is halted, a read API that stopped, the disk (absent when there is nothing to say or not measured yet) |
| `executor` | Only on a server that also runs the Executor (`kobd --executor`): its funds, pending and dropped transactions, last submission and clock alarm (`docs/ops/executor.md`, Part C *Monitoring*) |

```sh
curl -s "$API/v1/health/ready"
```

```json
{ "ready": true, "reason": "ok", "state": "following", "lag_seconds": 1 }
```
