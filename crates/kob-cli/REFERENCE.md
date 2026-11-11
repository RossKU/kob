# kob reference

The details behind [README.md](README.md): planner defaults, units, token resolution, fees, every order option, the `--json`
shapes, the exit codes, and the fine print of `kob watch`, `kob tokens` and the token commands. `kob <command> --help` is the
authority for options; this file matches the code at the time of writing.

KOB is in **open review** (no external audit). The option names and the JSON shapes below are a pre-release interface: they can
still change until a release says otherwise.

## Contents

- [The planner: same transactions as the web wallet](#the-planner-same-transactions-as-the-web-wallet)
- [Units](#units)
- [Which token a name means](#which-token-a-name-means)
- [Fees](#fees)
- [Network, node and safety checks](#network-node-and-safety-checks)
- [Order kinds: every option](#order-kinds-every-option)
- [JSON output](#json-output)
- [Exit codes](#exit-codes)
- [kob watch details](#kob-watch-details)
- [kob tokens details](#kob-tokens-details)
- [Token commands details](#token-commands-details)
- [kob merge, kob order and kob recover](#kob-merge-kob-order-and-kob-recover)

## The planner: same transactions as the web wallet

`kob place` is a port of the web app's planner (`web/src/kob/plan.ts` and `orders/`). For the same inputs it builds the same
order states, the same `createOrder` requests, and so the same transactions and `KOB1` placement records.
`crates/kob-cli/src/trade/tests_plan.rs` and `tests_plan_kinds.rs` replay the web planner's test cases and the golden vectors
(`crates/kob-protocol/vectors/golden.json`).

Defaults (the wallet's, `docs/spec/matcher.md` §10):

| Setting | Default |
|---|---|
| minimum fill (`--min-fill`) | worth 10 KAS at the limit; 1 base unit for IOC / FOK / market; `ceil(amount / 4)` for an if-done entry |
| stop trigger threshold (`--min-touch`) | max(minimum fill, 25 % of the amount) |
| carrier (`--carrier`) | 1 KAS on every covenant UTXO; at least 0.08 KAS and the token program's floor |
| tip (`--tip`, `--exit-tip`) | 0 on the KAS books; a pair order (`--quote`) with no tip carries the pair's keeper tip (`pair_keeper_tips.json`, the measured fee of one update, doubled) over its amount, rounded up, as the web ticket does (a crossing pair net pays its fee from the tips). A typed tip, `--tip 0` included, is kept and, below that default, prints a note that a crossing pair order may wait for a route through the KAS books |
| GTC lifetime | 90 days idle (renew with `kob replace <id> --renew`) |
| IOC / FOK life (`--life`) | 300 DAA; allowed 1..600 DAA |
| market order | an IOC auction from the best price over 200 DAA to a 3 % bound (`--slippage` changes the bound) |
| stop band | 3 % over 300 DAA |
| refund and keeper tips | the token program's |

Guards (also the wallet's): tick, value and 90-day bounds, self-trade, the FOK depth check, price-band warnings, and:

- **Crossing limits.** A limit that crosses the book becomes an auction by default (`--crossing auction|limit|reject`).
- **Market start.** The start of a market / close auction is the indexer's best price, and nothing independent checks it.
  Every market or close **sell** needs `--ack-market-start` on its own command line (`--yes` is not enough), unless
  `--floor-price P` bounds it: the auction then never goes below P (the bound is max(slippage bound, P)). A market buy needs
  it when it is worth 100 KAS or more or is far above the last fill.
- **Indexer checks.** Self-trade and book checks need the indexer; `--ack-no-guards` places the order when it cannot answer.
- Every UTXO the indexer names is looked up on the node before it is spent.
- The token outputs a command's own transaction creates (its token change) are recorded locally (`$KOB_STATE_DIR`, else
  `%LOCALAPPDATA%\kob` on Windows, `$XDG_STATE_HOME/kob` or `~/.local/state/kob`). The next command can spend them before the
  indexer has seen the transaction; they too are looked up on the node first.

**Prices are all-in.** A price is the order's limit per whole token; the tip comes on top: a sell receives `price - tip`, a buy
pays `price + tip`. The plan shows the all-in price and the all-in total of the whole amount by the covenant's rounding (a sell
receives at least the ceiling, a buy pays at most the floor).

## Units

Every amount carries its unit and nothing is rounded: a value that does not fit is refused with the nearest valid ones.
Units are not case sensitive.

| What | Units | Example |
|---|---|---|
| token amount | the token's ticker, `tok` / `token` / `tokens` (whole tokens, up to its decimals), or `base` (base units) | `12.5TUSD`, `1250000000base` |
| KAS amount | `KAS` (up to 8 decimals) or `sompi` (whole numbers) | `2KAS`, `200000000sompi` |
| price on a KAS book | KAS per whole token (`KAS` or `sompi`) | `0.25KAS`, `25000000sompi` |
| price on a pair A/B | B per whole A (B's ticker or `tok`, or `base`) | `2500TUSD` |
| tip | KAS per whole token | `0.001KAS` |
| basis points | `%` or `bps` | `3%`, `300bps` |
| duration | `s`, `m`, `h`, `d` or `daa` | `30s`, `600daa` |
| time | UTC `YYYY-MM-DDTHH:MM[:SS]Z` or `daa:N` | `2026-10-20T00:00Z` |

The unit is required everywhere, also on `kob send` and `kob take`. A bare `2.5` is refused with exit 2, for example
"the amount: `2.5` has no unit (amounts always carry one: e.g. 2KAS or 200000000sompi)".

Recipients of `kob send`: a token goes to a P2PK address or a 64-hex x-only key; KAS to any address of the network or a
64-hex key.

## Which token a name means

A token argument is a ticker or a covenant id. Sources, in order:

- **Registry.** `--registry FILE` (validated for the network), else the registry this build embeds for the same network. A
  registry token on a program pending review (one this build does not support) is not traded.
- **Indexer.** A token only the indexer knows is flagged `TOKEN_UNLISTED`: its ticker and decimals are the indexer's, so check
  its covenant id. A token of a reviewed program (on mainnet; on a test network any program the build embeds) is named by its
  covenant id once the indexer lists it (an order of it was placed).
- **Decimals of a token without a registry entry.** They come from the scale its orders use. A token without orders and
  without a KCC-23 declaration has none anywhere, so its first order names them: `--decimals N` (0..=9, the scale is 10^N).
  Once the token had an order, that scale decides; a `--decimals` that disagrees is refused.
- **Declared tokens (KCC-23).** The on-chain declaration comes first (`docs/spec/token-metadata.md` section 4). The CLI asks
  the indexer for the launch proof (`GET /v1/tokens/{id}/declaration`), verifies it against the covenant id (a proof that does
  not verify or names another id is refused), and takes the declared decimals (scale `10^min(decimals, 9)`) and symbol. A
  registry entry, token list, order scale or `--decimals` that disagrees is refused until they agree.
- **Declared but not listed yet.** A declared token the indexer does not list (no order of it) can be placed, sent or merged by
  covenant id when the wallet holds it: its program is the one the wallet's token UTXOs prove on the node. Any other covenant
  id the indexer does not list is refused (the web app opens such a market from a wallet that holds the token).
- **`kob send` of a token without `--indexer`.** Works when the local record covers the token: the registry, or the record
  this machine keeps for a day (in `%LOCALAPPDATA%\kob`, `$XDG_STATE_HOME/kob` or `~/.local/state/kob`) of what
  `kob token create` declared and of the key's own recent token outputs, each confirmed on the node. Anything else needs
  `--indexer`, and the error says so.
- **Ambiguous tickers.** A ticker that names more than one token is refused by every command that takes a token, with each
  candidate and its full covenant id. Give the covenant id.

## Fees

- A trading command pays the relay minimum: 100 sompi per gram, charged on the compute and transient mass, unless
  `--fee-rate N` says more (at least 100).
- Under contention the block template also ranks transactions by fee over their storage mass, which that fee does not cover.
  KOB transactions (their storage mass grows with the carrier) can then wait.
- `--priority-fee` is the web app's "Pay the priority fee": the fee is charged on the storage-inclusive mass (the largest of
  the compute, transient and storage mass; never below the relay minimum) at `--fee-rate` (default 100). It is higher, so it
  is opt-in. The plan then says "(priority fee, storage mass included)" and `transaction.fee_mode` is `"priority"` in `--json`.
  `kob order sweep|cancel` and `kob recover --cancel` take it too.
- **Congestion note.** When the node's fee estimate (`getFeeEstimate`) shows contended blocks and neither `--fee-rate` nor
  `--priority-fee` is given, one `note:` line on stderr says so and points at `--priority-fee`. `--fee-rate N` alone is still
  charged on the compute and transient mass only. The `--json` stdout is untouched; a node without the estimate gives no note.

## Network, node and safety checks

- **Network.** Mainnet unless `--testnet`, `--netsuffix=10` or `--network testnet-10` names testnet-10. A deployment build
  (`--features deploy-mainnet` or `deploy-tn10`, as in the release archives) is bound to its network and refuses the other.
  The plan starts with `network: <name>` and the prompt asks "Sign and submit ... on network: <name>?".
- **The node must run that network** (`getBlockDagInfo`; with the public nodes, both of the pair). A signature commits to no
  network, so a node of another one is refused before anything is planned from it, and again before anything is signed.
- **Public nodes.** Without `--node` or `$KOB_NODE_WRPC`, the Kaspa resolvers name two nodes on two hosts (Borsh wRPC over
  TLS). Whatever a transaction is planned from (the key's unspent outputs, the network) is read from both and must agree; an
  unreachable node is replaced by another. `$KOB_RESOLVERS` (comma-separated resolver URLs) replaces the resolver list.
- **Your own node.** JSON wRPC `ws://host:18110` (mainnet) or `:18210` (testnet-10); Borsh `ws://host:17110` / `:17210`, or
  `wss://.../borsh`. Put a URL with a password or API key in `$KOB_NODE_WRPC`, not on the command line.
- **Key content.** The key file (or variable) is read as is, surrounding whitespace is trimmed, and the rest must be 64 hex
  characters (either case) that form a valid secret key. A command line that contains the key's hex anywhere is refused. The
  CLI does **not** check the file's permissions or owner: protect it yourself (`chmod 600` on Unix, an owner-only ACL on
  Windows). `--key-file` wins over the environment. The one key rule of every command (`--key-file`, `--key-env`, `$KOB_KEY`) is
  in the README's [Keys](README.md#keys).
- **What you see before signing.** The plan (terms, carriers, fee, expiry) and the decoded transaction: each input and output
  with its owner, the net effect on the wallet in KAS and each token, the fee, the signatures asked for. A transaction that
  needs another key's signature is never signed.
- **Validation.** Every signed transaction runs in the rusty-kaspa script engine before it is submitted (and is written to
  `--out-dir` with its `submitRequest` when given). Node and indexer URLs are shown without credentials, paths or queries.

## Order kinds: every option

Every kind takes `--token`, `--quote`, `--side`, `--amount`, `--tip`, `--carrier`, `--decimals`, `--ack-no-guards`,
`--ack-market-start` and the common options. "Lifetime" means `--gtd WHEN` or `--day`, plus `--active-from WHEN`.
"Stop knobs" means `--min-touch AMOUNT`, `--band DURATION`, `--keeper-tip KAS`, `--min-rest DURATION`.

| Kind | Required | Optional |
|---|---|---|
| `limit`, `pair-limit` | `--price` (`pair-limit`: also `--quote`) | `--min-fill`, lifetime, `--crossing auction\|limit\|reject`, `--max-fills N` (a buy's delivery carriers) |
| `ioc`, `fok` | `--price` | `--min-fill`, `--life DURATION` (1..600 DAA), `--active-from` |
| `market` | | `--min-fill`, `--slippage BPS`, `--all-or-nothing`, `--floor-price P` |
| `streaming` | `--displayed-price`, `--tolerance BPS` | `--min-fill`, `--all-or-nothing` |
| `close` | (a sell; `--side` may be left out) | `--amount` (default: everything held), `--slippage`, `--all-or-nothing`, `--floor-price` |
| `twap` (sells), `dca` (buys) | `--price`, `--slice AMOUNT`, `--interval DURATION` | `--price-end` with `--slice-auction DURATION` (each slice an auction), `--min-fill`, lifetime; `dca`: `--max-fills` |
| `dutch` | `--price`, `--price-end`, `--duration` | `--step DURATION`, `--min-fill`, lifetime, `--max-fills` |
| `stop-market` | `--stop` | `--slippage`, `--min-fill`, `--max-fills`, stop knobs, lifetime |
| `stop-limit` | `--stop`, `--limit` | `--min-fill`, `--max-fills`, stop knobs, lifetime |
| `trailing-stop` | `--stop`, `--trail-step`, `--trail-gap` | `--trail-wait DURATION`, `--trail-updates N`, `--take-profit` (a trailing OCO), `--slippage`, `--min-fill`, `--max-fills`, stop knobs, lifetime |
| `take-profit` | `--take-profit` | `--min-fill`, `--max-fills`, lifetime |
| `oco` | `--take-profit`, `--stop` | `--limit`, `--slippage`, `--min-fill`, `--max-fills`, stop knobs, lifetime |
| `ifd` | `--price` and one of `--take-profit`, `--stop` | if-done options (below), `--min-fill`, lifetime |
| `ifo` | `--price`, `--take-profit`, `--stop` | as `ifd` |
| `repeat-ifd` | as `ifd` with `--take-profit` | `--repeat-count K` (default: unlimited within 90 days), as `ifd` |
| `repeat-ifo` | as `ifo` (the `--stop` ends the repeat) | as `repeat-ifd` |

If-done options: `--side` is the entry's side (buy = buy first and sell the exit later; sell = sell first and buy back).
`--prefund P` (sell first), `--entry-stop P` (a stop entry), `--limit P` (the exit stop-limit), `--exit-tip T`,
`--exit-min-fill`, `--exit-min-touch`, `--exit-slippage`, and a trailing exit stop with `--trail-step`, `--trail-gap`,
`--trail-wait`, `--trail-updates`.

Pairs: every kind takes `--quote TOKEN_B` (a `KobPair`, `KobCondPair` or `KobIfdPair` order). Prices are B per whole A; tips
stay in KAS (the default tip: see Defaults above). Taking pair orders and stop legs is not offered by `kob take` (an Executor fills them).

`kob replace` changes: plain limits take `--price`, `--tip`, `--amount`, `--add-kas`, `--renew` (in place when a sell keeps
its amount, or for a buy; otherwise a cancel-replace). Conditionals take `--price` (the take-profit), `--stop`,
`--stop-limit`, `--amount`, `--tip`, `--renew` (always a cancel-replace; an armed stop restarts unarmed). Trailing stops,
if-done entries and every pair order are re-planned from their terms with `--price`, `--take-profit`, `--stop`,
`--stop-limit`, `--exit-tip`, `--trail-step`, `--trail-gap`, `--entry-stop`. Repeat entries are not replaced (cancel instead).

## JSON output

`--json` is a flag of `place`, `replace`, `cancel`, `refund`, `take`, `send`, `merge`, `book`, `orders` and `balances`.
`kob tokens` prints one document with it and `kob watch` one JSON line per event. `kob order sweep|cancel` and `kob recover`
always print a JSON report. In `kob registry`, `programs`, `add-program` and `verify-genesis` take `--json`.
`kob token create|resume|verify-declaration` have no `--json` (text; the files `--out-dir` writes are JSON).

### Conventions

- Every amount of KAS or of a token is a **decimal string** in base units (sompi for KAS), e.g. `"2500000"`, negative for an
  outflow (as in `net`). No 64-bit value is rounded by a JavaScript parser.
- Covenant ids, transaction ids and keys are lower-case hex strings.
- Counts, indexes, DAA values (`due`, `lockTime`, `expiry_daa`), `scale` and `signatures` are JSON numbers; flags are
  booleans; an absent value is `null`.
- Strings meant for people (`action`, `inputs`, `outputs`, `price_text`, `what`, `text`, `disclosure`, an issue's `message`)
  have no fixed format: do not parse them.
- The order states and the `createOrder` request that `place` prints are the protocol library's JSON
  (`crates/kob-protocol/src/json.rs`: 64-bit integers as decimal strings, byte strings as hex, small integers as numbers).

In the examples `<64 hex>` stands for a 64-character hex string.

### Signing commands (`place`, `replace`, `cancel`, `refund`, `take`, `send`)

One JSON document on stdout once a transaction has been built: the plan, with the submission's outcome in `result`.

```json
{
  "action": "send 2.5 KAS to kaspatest:qq...",
  "network": "testnet-10",
  "plan": { "to": "kaspatest:qq...", "amount": "250000000" },
  "issues": [],
  "transaction": {
    "txid": "<64 hex>",
    "inputs": ["in  #0  your KAS 10 KAS"],
    "outputs": ["out #0  2.5 KAS to script <12 hex>..<8 hex>", "out #1  7.49997964 KAS to you"],
    "fee": "2036",
    "net": { "kas": "-250002036", "tokens": {} },
    "signatures": 1
  },
  "new_covenant_ids": [],
  "result": null
}
```

| Key | Type | Content |
|---|---|---|
| `action` | string | a title for people (`place limit sell TUSD`, `amend in place <id>`, `cancel-replace <id>`, `cancel <id>`, `refund <id>`, `take <id> (...)`, `send ... to ...`) |
| `network` | string | `mainnet` or `testnet-10` |
| `plan` | object | the command's own facts (below) |
| `issues` | array | findings, each `{"code": string, "severity": "error" \| "warning" \| "info", "message": string}`; an `error` stops the command before signing |
| `transaction.txid` | string | the transaction id |
| `transaction.inputs`, `.outputs` | arrays of strings | one text line per input and output |
| `transaction.fee` | string | the network fee in sompi |
| `transaction.fee_mode` | string | only with `--priority-fee`: `"priority"` |
| `transaction.net` | object | net effect on the key's wallet: `kas` (sompi, negative when KAS leaves, fee included) and `tokens` (covenant id to a signed base-unit string; tokens that net to zero are left out) |
| `transaction.signatures` | number | inputs the key signs |
| `new_covenant_ids` | array of strings | covenant ids the transaction creates (the new order(s) of a `place`, or of a `replace` that makes a new order); empty for cancel, refund, take, send |
| `result` | object or `null` | `null` unless the transaction was submitted |

`plan` per command:

| Command | `plan` keys |
|---|---|
| `place` | `states` (the order's state and, for a multi-leg kind, its further states), `request` (the web planner's `createOrder` request, or `null`) |
| `replace` | `order` (covenant id), `inPlace` (boolean), `replacement` (the new state), `value` (string, sompi on the replacement); a re-planned cancel-replace (trailing stop, if-done entry, pair order) adds `states` |
| `cancel` | `action` (`"cancel"`), `covenant_id`, `kind` (template name), `returned` (array of `{"token", "units", "utxos", "kas"}`: the strays it takes back; `utxos` is a number) |
| `refund` | `order`, `due` (number, DAA), `earned` (string, sompi, negative when the refund costs KAS) |
| `take` | `order`, `amount` (string, base units), `lockTime` (number) |
| `send` of KAS | `to` (as typed), `amount` (string, sompi) |
| `send` of a token | `to`, `token` (covenant id), `amount` (string, base units), `carrier` (string, sompi on the recipient's token output) |

After a successful submission:

```json
{ "status": "submitted", "txid": "<64 hex>", "fee": "2036", "covenant_ids": [], "file": null, "engine_validated": true }
```

`status` is always `"submitted"` (a failure leaves `result` `null` and exits 1). `covenant_ids` repeats `new_covenant_ids`;
`file` is the path written under `--out-dir`, else `null`; `engine_validated` is always `true`.

The document is printed once, when the outcome is known, so stdout is a single pretty-printed JSON value (not a stream).
`kob cancel A B` handles the ids one after the other and prints one document per id (read them as a stream of JSON values;
`jq -s` slurps them); one id's failure does not stop the next.

### What `--yes` and `--dry-run` change

`--yes` together with `--dry-run` is refused (exit 2).

| Invocation | stdout | stderr | Exit |
|---|---|---|---|
| `--json --dry-run` | the document, `result` null | `dry run: nothing signed or submitted` | 0 |
| `--json --yes` | the document, `result` filled | nothing (a warning if the node reports another transaction id) | 0 |
| `--json`, no `--yes`, no terminal | the document, `result` null | `not signed: pass --yes to sign and submit ...` | 1 |
| `--json`, no `--yes`, a terminal | the document, `result` null if not signed | the plan as text, then `Sign and submit ... Type yes to continue:`; `yes` or `y` signs, anything else prints `not signed: cancelled` | 0 or 1 |
| a plan with an `error` issue | the document, `result` null | `error: the plan has errors: nothing signed` | 1 |
| needs another key's signature | the document, `result` null | `error: the transaction needs signatures of other keys: nothing signed` | 1 |
| signing, validation or submission failed | the document, `result` null | `error: ...` | 1 |
| nothing could be planned (node or indexer error, not enough KAS or tokens, bad terms) | nothing | `error: ...` | 1, or 2 for a usage error |

With `--json` the findings are in `issues` and not also on stderr; without it they go to stderr as
`severity: message [CODE]`. Exception: when `place` cannot build any transaction, it prints the findings on stderr as text
even with `--json`, and stdout stays empty.

### `merge`

One summary document with a `links` array of the links submitted (empty for `--dry-run`, and when it stops before the first;
a link that fails leaves the earlier ones in the array and exits 1, and the message says to run `kob merge` again).

```json
{
  "action": "merge TUSD",
  "network": "testnet-10",
  "token": "<64 hex>",
  "utxos_before": 3,
  "utxos_after": 1,
  "transactions": 1,
  "merged_amount": "3500000000",
  "fee": "4500",
  "links": [{ "link": 1, "status": "submitted", "txid": "<64 hex>", "fee": "4500" }]
}
```

`utxos_before`, `utxos_after` and `transactions` are numbers; `merged_amount` (base units) and `fee` (the whole chain's,
sompi; each link carries its own) are strings.

### Views (`book`, `orders`, `balances`)

One document on stdout; nothing is signed. `--yes` and `--dry-run` are accepted and have no effect. A `book` of a KAS market:

```json
{
  "token": "<64 hex>",
  "quote": null,
  "network": "testnet-10",
  "scale": 100000000,
  "asks": [{ "price": "25000000", "price_text": "0.25 KAS per TUSD", "amount": "500000000", "orders": 2 }],
  "bids": [{ "price": "24500000", "price_text": "0.245 KAS per TUSD", "amount": "300000000", "orders": 1 }],
  "best_ask": "25000000",
  "best_bid": "24500000",
  "crossed": false,
  "last_fill": "24800000"
}
```

- `book`: `token`; `quote` (covenant id of B for a pair, else `null`); `network`; `scale` (number, the order scale; `null`
  for a token without orders and without known decimals); `asks` and `bids` (levels: `price` is the state price as a
  string, sompi per `scale` base units on a KAS book or B base units on a pair; `price_text` the same per whole token;
  `amount` a base-unit string; `orders` a number); `best_ask`, `best_bid`, `last_fill` (state-price strings, `null` when
  none; `last_fill` is always `null` on a pair); `crossed` (boolean) is `true` when the best bid is above the best ask, which an
  executor is about to match: the text then says `crossed by X` instead of a spread.
- `orders`: `{"maker", "network", "orders": [...]}`. Each order has `covenant_id`, `contract` (template name), `market`
  (ticker or `A/B`), `status` (the indexer's), `what` (text), `expiry_daa` (number or `null`), `flags` (strings such as
  `expired`, `refundable from DAA N`, `possibly frozen`) and `value` (the indexer's `current.value` as sent, `null` when
  absent).
- `balances`: `{"maker", "kas": {"free", "locked", "total"}, "tokens": [...], "skipped": [...]}`, the KAS values sompi
  strings. Each token row has `token`, `ticker`, `decimals` (number or `null`), `free`, `in_orders`, `strays`,
  `kas_escrow`, `kas_carriers`, `kas_locked` (strings, base units or sompi), `free_utxos` (number), `free_verified`
  (boolean: the free tokens were confirmed on the node, else they are the indexer's figure), `orders` (number) and `text`
  (the same for people). `skipped` lists orders not counted: `{"covenant_id", "reason"}`.

### `kob tokens`

`{network, indexer, search, tokens: [{ticker, covenant_id, decimals, program, open_asks, open_bids, warnings, source}],
kascov, note}`. `source` is `indexer` or `kascov`; the counts are `null` for a kascov-only token.

### `kob watch`

NDJSON, one document per line:

- a first `{"type":"start","data":{"channels":[..],"indexer":..,"network":..}}` line;
- every frame the server sends, exactly as sent (`fill`, `book`, `order`, `cursor`, `health`, `reorg`, `resync`, `error`);
- `{"channel":"book:<id>"|"order:<id>","type":"snapshot","data":{..}}` for each re-read (`data` is `kob book --json`'s
  document, or the row `kob orders --json` prints for the order; `null` for an order the indexer does not know);
- `{"type":"note","data":{"message":".."}}` for the watch's own events (reconnects, reorgs).

### `kob order sweep|cancel` and `kob recover`

A report on stdout (or `--out FILE`); one line per order and a summary go to stderr. Confirmation is per order (`kob recover`
only with `--cancel`): `--yes` signs and submits; without it a terminal is shown the order and asked; without a terminal the
order is `refused` (reason `not signed: pass --yes ...`) and the exit code is 1. `--dry-run` signs and engine-validates but
never submits.

- `kob order`: `{"action": "sweep" | "cancel", "network", "orders": [...], "rejected": [{"source", "reason"}]}`. Each order
  has `covenant_id`, `kind`, `source` and `status`: `submitted`, `signed` (a dry run), `skipped`, `refused` or `failed` (the
  last two with `reason`). A `submitted` or `signed` order also has `txid`, `fee` (string), `funding` (`"order"`, or the
  outpoint of the maker's UTXO that paid the fee), `returned` (as `cancel`'s), `left_behind` (array of
  `{"outpoint", "token", "reason"}`), `disclosure` (text), `file` (path or `null`) and `engine_validated`.
- `kob recover`: `{"network", "node", "orders": [...], "rejected": [...], "summary": {"live", "not_found", "unsupported",
  "other_maker", "rejected"}}` (numbers in `summary`). Each order has `covenant_id`, `kind`, `template`, `template_hash`,
  `maker`, `status` (`live`, `not_found`, `unsupported` or `other_maker`), `note`, `source` and `candidates`; a live one also
  `outpoint`, `value`, `utxo_daa`, `state`, `amount_left`, `scale`, `min_fill` (strings), `found_by` and `custody`, and with
  `--cancel` a `cancel` object shaped like an order of `kob order`.

### `kob registry`

`programs --json` prints the array of token-program profiles. `add-program --json` prints one document (`profile`,
`detection`, `already_embedded`, `blocked`, `review_tasks`, `registry_template`). `verify-genesis --json` prints its text
lines first and then the array of genesis records (`ticker`, `covenant_id`, `genesis_verified`, and `genesis` or `error`) on
the same stdout, so its output is not one JSON document.

## Exit codes

The code separates three outcomes; the finer cause is in the message on stderr, never in the number.

| Code | Meaning |
|---|---|
| 0 | success: the work was done, `--dry-run` stopped after the plan, a view was printed, or `--help` / `--version` |
| 1 | the command ran and did not do its work: a node, indexer or network-check error, a planning error, a plan with an `error` issue, a transaction needing another key's signature, a signing / script-engine / submission failure, **or a refusal to sign** (no `--yes` without a terminal, or an answer other than `yes`) |
| 2 | usage: an unknown command or option, a missing or malformed argument, no signing key, a key on the command line (`--key` of a trading command), `--yes` with `--dry-run`, `kob place` without a kind; also an order term that does not read (an amount, price, tip or carrier without a unit or out of range, a covenant id that is not 64 hex, a missing required option of the kind) in `place`, `replace`, `take`, `send` and `refund` |

So a refusal, a plan error, a node error and a declined prompt are all 1. Read stderr (`error: ...` for failures,
`not signed: ...` for a refusal or a declined prompt) or the `issues` array. A mistake in the arguments is 2 on every command,
the views (`book`, `orders`, `balances`) included (no token, `--depth 0`, no key and no `--maker`); their indexer and node
failures are 1.

An explicit `--help` or `-h` prints the usage on stdout and exits 0, on every command and subcommand: `kob --help`,
`kob token --help`, `kob registry --help`, `kob registry validate --help`, `kob book --help`. `kob help <command> [<subcommand>]` is
`kob <command> [<subcommand>] --help`. `kob` with no command and `kob help` print the usage on stdout and exit 0. A usage
printed because of a mistake goes to stderr and exits 2: an unknown command or subcommand, `kob token` or `kob registry` with
no subcommand, and `kob place`, `kob order`, `kob watch` and `kob token resume` with no arguments. `kob --version` (`-V`)
prints `kob 0.1.0 (testnet-10 build, commit a1b2c3d4e5f6)`; a build without a deployment feature says `development build, no
network`, and the commit is `unknown` when the build was not given one.

Per family:

- **Trading commands.** `kob cancel A B` tries every id; the exit code is that of the last id that failed (0 when none did).
- **`kob order sweep|cancel`, `kob recover`.** 0 when every attempted order was handled; 1 when an order is `failed` or
  `refused` (for `kob order` also when a file was `rejected`), when the report could not be written, or on an error such as
  an unreadable file or an unreachable node; 2 for an argument error (`--yes` with `--dry-run` included). An order that is
  only `skipped` does not make the code 1. `kob recover` without `--cancel` is 0 whatever it found.
- **`kob token create`.** 0 on success or a dry run; 2 for an argument error (an unknown or malformed option, a missing
  required one, no key); 1 on any other error and on a refusal (no `--yes`, no terminal). `resume`: the same, and 2 with no
  argument. `verify-declaration`: 0 when the proof verifies, 1 when it does not or
  cannot be read, 2 for a wrong number of arguments.
- **`kob tokens`.** 0 when the list was printed, 1 on an error, 2 for an argument error.
- **`kob watch`.** 0 after Ctrl-C (or when the output pipe closes); 1 for a fatal error (no WebSocket route, credentials
  wanted, every subscription refused); 2 for an argument error.
- **`kob registry`.** `validate`: 0 valid, 1 invalid, 2 usage or an unreadable file. `verify-genesis`: 0 when the registry
  says exactly what the evidence proves, 1 on a mismatch or an invalid registry or evidence file, 2 for usage or an
  unreadable file. `programs`: 0, or 2 for usage. `add-program`: 0, 1 when the profile cannot be derived or `--write` is
  refused, 2 for usage. Every one answers `--help` / `-h` with its usage on stdout and exit 0.

## kob watch details

- **Address.** Derived from the indexer (`--indexer`, `$KOB_INDEXER_URL`, else the network's default): `http://host:port`
  becomes `ws://host:port/v1/ws`, `https://` becomes `wss://`; a base path is kept.
- **Fill line.** `time (UTC)  fill  ask|bid  amount @ price  [paid KAS]  order <short id>  tx <txid>  daa N`. Amount and price
  are in the token's decimals (base units when nothing tells them); `ask` means a sell order was filled.
- **Book and order notices** carry no content: the CLI reads `GET /v1/books/{token}` or `GET /v1/orders/{id}` again (the same
  text as `kob book` / `kob orders`), at most every 2 s and once per burst. Every watch also reports chain reorganisations
  (a note, and a new read of what is shown). `health` prints a cursor line per published batch and a health line every 15 s.
- **Keepalive and reconnects.** The CLI pings every 25 s (the server closes a session silent for 60 s). When the connection
  drops it prints a note, waits 1 s, 2 s, 4 s ... up to 30 s (a `Retry-After` of a refused upgrade is honoured), reconnects,
  subscribes again and re-reads what is shown. The server replays nothing: fills during the gap are not shown. A `resync`
  frame (this client fell behind the server's buffer) also triggers a note and a re-read.
- **Errors.** A refused subscription (`unknown channel`, `invalid channel`) is printed with the server's reason; when every
  channel is refused the watch exits 1. An endpoint without `/v1/ws` (404) or one that wants credentials (401 / 403) exits 1.
- `kob watch` refuses `--yes`, `--dry-run`, `--key-file`, `--key-env`, `--fee-rate`, `--priority-fee`, `--out-dir` and
  `--node` (exit 2).

## kob tokens details

- **Default list** is the indexer's `GET /v1/tokens` (`docs/api-reference.md` 3.4): every token an order was placed for, plus the
  registry's. It reads no key and no node.
- **Columns.** `TICKER` (the registry's, else the symbol of the token's verified KCC-23 declaration, else blank),
  `COVENANT ID` (always whole: tickers collide, the id is the identity), `DEC`, `PROGRAM` (program and slots, such as
  `kcc20-ref-public-mint 3/3`), `ASKS`, `BIDS` (live orders) and, when some row has one, `WARN`: the indexer's `warnings`
  and `powers` (`live_mint_authority`, `freeze`, ...; `genesis_unverified` is left out). There is no "verified" or
  "official" column: KOB makes no judgement of a token.
- **`--all`** adds every token kascov.io lists for the network (`https://kascov.io/data/<network>/tokens.json`). The ticker
  (`claimed_ticker`) and decimals (`claimed_decimals`) are the token's own claims; `ASKS` / `BIDS` are `-`; `PROGRAM` is the
  KOB program the token's program hash names (`standard.view_template_hash`, else `standard.template_hash`; a program KOB
  lists but has not reviewed says so), `unsupported` for a hash KOB has no program for, `unidentified` when kascov gives
  none. A token the indexer lists is shown once, with the indexer's facts.
- **Fetching.** Only when you run the command, never in the background. The local copy (`$KOB_CACHE_DIR`, else
  `%LOCALAPPDATA%\kob\cache`, `$XDG_CACHE_HOME/kob` or `~/.cache/kob`) is reused for the server's `Cache-Control: max-age`
  (30 s when none is given); after that a conditional request (`If-None-Match` / `If-Modified-Since`), so an unchanged list
  costs a 304. `--refresh` fetches now (it implies `--all`). When kascov.io is unreachable the older copy is shown with a
  note; with no copy the command says so, and `kob tokens` without `--all` still works.
- A full covenant id in no list is reported as such: kascov may not have indexed it yet, and the other commands can still
  read it (`kob book <id>`).

## Token commands details

`kob token create` makes two transactions (KCC-23): a **preparation** that spends the funding into one output of the funding
key and declares the metadata in its payload, then the **genesis** that spends that output, so the covenant id commits to the
declaration. Both are signed at once; the genesis is sent right after the preparation.

| Option | Meaning |
|---|---|
| `--name`, `--ticker`, `--decimals` | required: name (1..=64 chars), ticker (2..=12 of A-Z 0-9), decimals (0..=18) |
| `--fund-utxo TXID:INDEX:AMOUNT:OWNER` | required, repeatable, all of one key; AMOUNT in sompi; OWNER a 64-hex x-only key or a P2PK address |
| `--supply UNITS` | total supply in base units; must equal the sum of the holders. Without `--holder` it all goes to the funding key |
| `--holder OWNER:SCHEME:AMOUNT[:BORROW_SCHEME:GUARD]` | repeatable, one token output each (OWNER 64-hex, `self` or an address) |
| `--program` | `public-mint` (default), `3x3`, `8x8-prototype`, `kasparocket-testnet` (testnet-10 only) |
| `--carrier` | sompi on each token output (default 1000000000 = 10 KAS) |
| `--fee`, `--fee-rate`, `--change-to` | explicit genesis fee; sompi per mass unit (default 100); change address (default the funding key) |
| `--description`, `--image`, `--website`, `--x`, `--telegram` | declared on chain (at most 1024 bytes with name, symbol, decimals); `--logo` / `--icon` are deprecated spellings of `--image` |
| `--image-file PATH` / `--image-hash HEX` | the picture's SHA-256, declared as `image_hash` (needs `--image`; PNG, JPEG, WebP or GIF up to 1500000 bytes, no SVG) |
| `--extension-commitment`, `--allow-borrow` | advanced; see `--help` |
| `--key-file PATH`, `--key-env NAME` | the signing key (the README's [Keys](README.md#keys); else `$KOB_KEY` when the run signs). The older `--key file:PATH`, `--key env:VAR` and `--key HEX` still work and print a deprecation warning |
| `--out-dir`, `--node`, `--network`, `--yes` / `-y`, `--dry-run` | as below |

- **Program.** The default `public-mint` issues the `KCC20` actor of upstream's published `KCC20PublicMint` app (registry
  `kcc20-ref-public-mint`, 3 / 3 slots), which indexers such as kascov.io list as a KCC-20 token. The genesis holds only token
  holders: nobody can mint more. `--program 3x3` issues the standalone reference program (`kcc20-ref-3x3`): a transfer costs
  less, but other indexers (kascov.io) do not recognise the token, and the plan says so. Measured on testnet-10 on
  2026-11-04: issuance 0.00415 KAS for both; a 1 -> 1 transfer 0.00901 KAS (public-mint) against 0.00678 KAS (3x3). Tokens
  already issued as 3 / 3 stay tradable.
- **Dry run.** `--dry-run` builds and verifies everything offline and never submits. With a key it signs and runs every
  input in the script engine; without it the transactions stay unsigned and script execution is skipped (the output says
  so). Nothing is written unless `--out-dir` is given.
- **Submitting.** Without `--dry-run`, a key is required (`--key-file`, `--key-env` or `$KOB_KEY`). The node is `--node`, else `$KOB_NODE_WRPC`, else the public nodes,
  and it must run the named network. The plan is shown and confirmed like the trading commands: `--yes` submits, a terminal
  is asked, without either it stops with `error: not signed: pass --yes ...` (exit 1) before anything is written or sent.
- **Files.** `preparation_tx.json`, `genesis_tx.json`, `submit_preparation.json`, `submit_request.json`, `supply.json`,
  `metadata.json`, `metadata.payload.hex`, `launch_proof.json`, `registry_entry.json` and `creation.json` go to `--out-dir`,
  else (when submitting) to `./kob-create-<first 16 hex of the covenant id>`.
- **`kob token resume <DIR> [--node URL] [--network NET]`** sends a creation whose genesis did not go out, from the directory
  `kob token create` wrote. Nothing is signed again: the preparation is sent first (a node that already has it refuses it,
  which is fine), then the genesis. It refuses unsigned transactions (a dry run without a key) or a genesis that does not
  spend output 0 of the preparation. When the genesis fails, `kob token create` prints this command.
- **`kob token verify-declaration <launch_proof.json>`** checks a KCC-23 launch proof from any source (an indexer's
  `GET /v1/tokens/<id>/declaration`, a community list, `kob token create`). It recomputes both transaction ids and the
  covenant id and prints the covenant id, the transaction ids and the declaration only when it is bound to that id (exit 0);
  otherwise it prints the error on stderr and exits 1.
- `kob token issue` is the former name of `kob token create`: it still works, identically, with a one-line deprecation note on
  stderr.

## kob merge, kob order and kob recover

- **`kob merge <TOKEN> [--wait-secs N]`** takes the key's plain UTXOs of the token (smallest first) and chains transfers to the
  key itself, at most 10 transactions per run (run again for more). The merged output keeps the largest input carrier (at most
  1 KAS); the freed carriers pay the fee. One confirmation signs the chain; each later link waits up to `--wait-secs`
  (default 30) for the node to show the previous output.
- **`kob order sweep --view FILE...`** returns an order's strays to the maker while the order continues in place (same
  covenant id). **`kob order cancel --view FILE...`** is the maker's cancel from saved indexer views (an order under a retired template of
  the cancel-only table too: through that template's own cancel entry; `kob order sweep` skips such an order). Views come from `GET /v1/orders/{id}` or a page such as `GET /v1/orders?maker=<pubkey>&status=active`.
- **`kob recover --from FILE... [--amount-left [ID=]AMOUNT] [--cancel --key-file PATH]`** finds the maker's orders on a node alone,
  from the web wallet's backup, an indexer recovery / export file or an indexer view. The search is not exhaustive: an order
  partly filled by an unusual amount is reported `not_found` unless you pass `--amount-left`. `--cancel` cancels every live
  order of the key's maker (strays are not swept: use `kob order` from an indexer view first). `getUtxosByAddresses` needs a
  node with `--utxoindex`.
- These three keep their own option parsers, but the key rule is the same as everywhere: `--key-file PATH`, `--key-env NAME`,
  else `$KOB_KEY`. The older `--key file:PATH`, `--key env:NAME` and `--key HEX` are still accepted and print a deprecation
  warning that names the new spelling (a `HEX` key is visible in the process list and shell history).
