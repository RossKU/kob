# kob (kob-cli)

The KOB command line. `kob --help` lists every command, `kob <command> --help` its options (`kob token issue --help`,
`kob token resume --help`, `kob token verify-declaration --help`; `kob token --help` and `kob registry --help` are not commands, see
"Token commands").

**Names.** The binary is built from the package `kob-cli`, which declares no `[[bin]]` name, so the file is called
`kob-cli` (`kob-cli.exe` on Windows): the release archive `kob-executor-<version>-<network>-<target>` holds `kob-executor` and
`kob-cli` (`scripts/package-release.mjs`), and `docs/ops/executor-quickstart.md` runs it as `kob-cli`. The text inside the
program calls it `kob`: `kob --help`, every usage text and `--version` (which prints `kob <version>`) say `kob`, and so does this
file. Run `kob-cli place ...` where this file says `kob place ...`, or rename or alias the file to `kob`. The argument parser is
hand-written (there is no clap), so help is per command; `kob registry validate` and `kob registry programs` take no `--help` (a
wrong argument prints their usage line and exits 2).

| Command | What it does |
|---|---|
| `kob place <KIND>` | place an order, planned exactly like the web wallet's order ticket |
| `kob replace <ID>` | amend a live order in place, or cancel-replace it in one transaction |
| `kob cancel <ID>...` | the maker's cancel by covenant id (custody, strays and KAS back to the maker) |
| `kob refund <ID>` | the permissionless refund of an order that has ended (the sender keeps the refund tip) |
| `kob take <ID> --amount A` | fill a resting order as the taker |
| `kob send <TOKEN\|KAS> <TO> <AMOUNT>` | send a token (within its program's slots) or KAS |
| `kob merge <TOKEN>` | merge the key's own UTXOs of a token into one (a chain of transfers to itself) |
| `kob book <TOKEN> [--quote B]` | the order book on KAS or of a pair (the indexer's view) |
| `kob orders` | the maker's orders, live or (`--all`) every status |
| `kob balances` | free KAS and tokens, and what the maker's live orders hold (custody, strays, KAS locked) |
| `kob order sweep\|cancel --view FILE` | the maker's stray sweep in place and cancel from saved indexer views |
| `kob recover --from FILE` | find the maker's orders from a backup / export / view on a node alone, and cancel them |
| `kob token issue` | issue a fixed-supply KCC-20 token (a KCC-23 declaration, then the genesis) |
| `kob token resume <DIR>` | send a creation whose genesis did not go out, from the files `kob token issue` saved |
| `kob token verify-declaration <PROOF>` | check a KCC-23 launch proof from any source and print the declaration |
| `kob registry validate\|verify-genesis\|programs\|add-program` | the token registry and token-program profiles |

## The trading commands

`place`, `replace`, `cancel`, `refund`, `take`, `send` and `merge` read the node (wRPC JSON, `--node` or `$KOB_NODE_WRPC`) and the
indexer (`--indexer` or `$KOB_INDEXER_URL`, `http://`; `docs/ops/executor.md` Part B), plan the transaction, print it and
sign only after confirmation.

**Same transactions as the web wallet.** `kob place` is a port of the web app's planner (`web/src/kob/plan.ts` and
`orders/`): the same order states, the same `createOrder` requests and therefore the same transactions and `KOB1`
placement records for the same inputs. Its defaults are the wallet's (`docs/spec/matcher.md` §10): a minimum fill worth
10 KAS at the limit (1 base unit for IOC / FOK / market, `ceil(amount / 4)` for an if-done entry), 2 KAS on every covenant
UTXO (`--carrier`, at least 0.08 KAS and the token program's floor), tip 0, GTC = 90 days idle, IOC / FOK life 300 DAA,
market orders as IOC auctions from the best price over 200 DAA to a 3 % bound, stop bands of 3 % over 300 DAA, the
program's refund and keeper tips. The guards are the wallet's too: tick, value and 90-day bounds, self-trade, the FOK
depth check, a limit that crosses the book becomes an auction (`--crossing auction|limit|reject`), price-band warnings,
a market start checked against the last fill (`--ack-market-start`; the start is the indexer's best price and nothing
independent checks it, so every market / close sell needs `--ack-market-start` on its own command line, `--yes` is not
enough, unless `--floor-price P` bounds it: the auction then never goes below P), self-trade and book checks that need the
indexer (`--ack-no-guards` when it cannot answer). Every UTXO the indexer names is looked up on the node before it is spent.
The token outputs a command's own transaction creates (its token change) are recorded locally (`$KOB_STATE_DIR`, else the
per-user state directory), so the next command can spend them before the indexer has seen the transaction; they too are
looked up on the node first.
`crates/kob-cli/src/trade/tests_plan.rs` and `tests_plan_kinds.rs` replay the web planner's test cases (every kind: the plain,
conditional and if-done kinds, on KAS and on a pair) and the golden vectors those tests compare with
(`crates/kob-protocol/vectors/golden.json`).

**Prices are all-in.** A price is the order's limit per whole token; the tip comes on top of it: a sell receives
`price - tip`, a buy pays `price + tip`. The plan shows the all-in price and the all-in total of the whole amount by the
covenant's rounding (a sell receives at least the ceiling, a buy pays at most the floor).

**Tokens without a registry entry.** A token of a reviewed program (on mainnet; on a test network any program the build
embeds) is named by its covenant id once the indexer lists it (an order of it was placed). Its decimals come from the
scale its orders use; a token without orders and without a KCC-23 declaration has none anywhere, so its first order names
them: `--decimals N` (0..=9, the scale is 10^N).

**Declared tokens (KCC-23).** A token's on-chain declaration comes first (`docs/spec/token-metadata.md` section 4): the
CLI asks the indexer for its launch proof (`GET /v1/tokens/{id}/declaration`, for a token its list marks declared or does
not list), verifies it against the covenant id (a proof that does not verify or names another id is refused), and takes
the declared decimals (the scale is `10^min(decimals, 9)`) and symbol. A registry entry or a token list naming other
decimals, a market whose orders use another scale, or a `--decimals` of another scale is refused until they agree. A
declared token the indexer does not list yet (no order of it) is placed, sent or merged by covenant id when the wallet
holds it: its program is the one the wallet's token UTXOs prove on the node. Any other covenant id the indexer does not
list is refused: the CLI has nothing to check its program against (the web app opens such a market from a wallet that
holds the token).

**Units.** Every amount carries its unit and nothing is rounded: a value that does not fit is refused with the nearest
valid ones.

| what | units | example |
|---|---|---|
| token amount | the token's ticker or `tok` (whole tokens, up to its decimals), `base` (base units) | `12.5TUSD`, `1250000000base` |
| KAS amount | `KAS` (up to 8 decimals), `sompi` | `2KAS`, `200000000sompi` |
| price on a KAS book | KAS per whole token | `0.25KAS` |
| price on a pair A/B | B per whole A (B's ticker or `tok`, or `base`) | `2500TUSD` |
| tip | KAS per whole token | `0.001KAS` |
| basis points | `%` or `bps` | `3%`, `300bps` |
| duration | `s`, `m`, `h`, `d` or `daa` | `30s`, `600daa` |
| time | UTC `YYYY-MM-DDTHH:MM[:SS]Z` or `daa:N` | `2026-10-20T00:00Z` |

The unit is required everywhere, also on `kob send` and `kob take`: `kob send KAS <TO> 2.5KAS` or `2.5kas` (units are not case
sensitive), and `kob send TUSD <TO> 12.5TUSD`; a bare `2.5` is refused (the error reads "the amount: `2.5` has no unit (amounts always carry
one: e.g. 2KAS or 200000000sompi)"; for a token the hint names its ticker and `base`). A KAS amount takes `KAS` (up to 8
decimals) or `sompi` (whole numbers); a token amount takes the ticker, `tok` / `token` / `tokens` or `base`. A token goes to
a P2PK address or a 64-hex key; KAS to any address of the network or a 64-hex key.

**Safety.**

* The signing key comes from `--key-file PATH` (a file holding the 64-hex secret) or the environment (`$KOB_KEY`, or
  `--key-env NAME`). `--key` is refused, and so is any argument containing the key: arguments are visible to other
  users (`ps`, shell history). The key is never printed. What the code checks about the key is its content: the file (or
  the variable) is read as it is, surrounding whitespace is trimmed, and the rest must be 64 hex characters (either case)
  that are a valid secret key. It does **not** check the file's permissions or owner and does not warn about a file that
  other users can read: protect it yourself (`chmod 600` on Unix, an owner-only ACL on Windows). `--key-file` wins over the
  environment when both are given. The token commands, `kob recover` and `kob order` read their key differently (below).
* Every command prints the plan (the order's terms, carriers, fee, expiry) and the decoded transaction (each input and
  output with its owner, the net effect on the wallet in KAS and each token, the fee, the signatures asked for) before
  anything is signed. `--dry-run` stops there. Without `--yes` a terminal is asked (`yes` to continue); without a
  terminal the command refuses. A transaction that needs another key's signature is never signed.
* Mainnet is refused unless named: `--network mainnet`, even by a mainnet deployment build. The node must run the
  network named (`getBlockDagInfo`): a signature commits to no network, so a node of another one is refused before
  anything is planned from it, and again before anything is signed.
* Tokens are read from the registry (`--registry FILE`, validated for the network, else the embedded one of the same
  network) and the indexer. A registry token on a program pending review (one this build does not support) is not traded; a token only
  the indexer knows is flagged `TOKEN_UNLISTED` (its ticker and decimals are the indexer's: check its covenant id).
* Every signed transaction is validated in the rusty-kaspa script engine before it is submitted (and written to
  `--out-dir` with its `submitRequest`). Node and indexer URLs are shown without credentials, paths or queries.

### Examples

```sh
export KOB_NODE_WRPC=ws://127.0.0.1:18210 KOB_INDEXER_URL=http://127.0.0.1:8090
N="--network testnet-10 --key-file ~/.kob/maker.key"

# limit orders: GTC, a GTD date, a day order, a timed start
kob place limit --token TUSD --side sell --amount 12.5TUSD --price 0.25KAS $N --dry-run
kob place limit --token TUSD --side buy  --amount 100TUSD  --price 0.24KAS --tip 0.0001KAS --gtd 2026-10-20T00:00Z $N --yes
kob place limit --token TUSD --side sell --amount 5TUSD --price 0.26KAS --day --active-from 2026-10-14T09:00Z $N
# IOC / FOK, a market order (an IOC auction from the best price to 3 %, or --slippage 1%)
kob place ioc    --token TUSD --side buy --amount 40TUSD --price 0.25KAS $N
kob place fok    --token TUSD --side sell --amount 40TUSD --price 0.24KAS --life 30s $N
kob place market --token TUSD --side sell --amount 40TUSD --slippage 1% $N
# stops, take-profit, OCO
kob place stop-market --token TUSD --side sell --amount 40TUSD --stop 0.22KAS $N
kob place stop-limit  --token TUSD --side sell --amount 40TUSD --stop 0.22KAS --limit 0.215KAS $N
kob place take-profit --token TUSD --side sell --amount 40TUSD --take-profit 0.30KAS $N
kob place oco         --token TUSD --side sell --amount 40TUSD --take-profit 0.30KAS --stop 0.22KAS $N
# if-done: buy first and sell the exit at 0.30, or sell first and buy back at 0.22; ifo = both exit legs
kob place ifd --token TUSD --side buy  --amount 40TUSD --price 0.25KAS --take-profit 0.30KAS $N
kob place ifd --token TUSD --side sell --amount 40TUSD --price 0.26KAS --take-profit 0.22KAS $N
kob place ifo --token TUSD --side buy  --amount 40TUSD --price 0.25KAS --take-profit 0.30KAS --stop 0.22KAS $N
# streaming (from the price you saw, within 0.5 %), close (sell everything held, a market auction)
kob place streaming --token TUSD --side buy --amount 40TUSD --displayed-price 0.25KAS --tolerance 0.5% $N
kob place close     --token TUSD $N
# TWAP / DCA: 5 TUSD every 10 minutes (each slice an auction from 0.26 down to 0.25 over 20 s); a Dutch decay
kob place twap  --token TUSD --amount 40TUSD --slice 5TUSD --interval 10m --price 0.26KAS --price-end 0.25KAS --slice-auction 20s $N
kob place dca   --token TUSD --amount 40TUSD --slice 5TUSD --interval 1h --price 0.24KAS $N
kob place dutch --token TUSD --side sell --amount 40TUSD --price 0.30KAS --price-end 0.25KAS --duration 2h $N
# a trailing stop 0.03 below the market, moving in steps of 0.005 (with a take-profit: a trailing OCO)
kob place trailing-stop --token TUSD --side sell --amount 40TUSD --stop 0.22KAS --trail-step 0.005KAS --trail-gap 0.03KAS $N
# a stop entry, a repeat IFD (re-buys after every take-profit, 5 times)
kob place ifd        --token TUSD --side buy --amount 40TUSD --price 0.26KAS --entry-stop 0.255KAS --stop 0.22KAS $N
kob place repeat-ifd --token TUSD --side buy --amount 40TUSD --price 0.25KAS --take-profit 0.30KAS --repeat-count 5 $N
# pairs: every kind with --quote (prices in B per whole A, the tip in KAS): a limit, a stop, an if-done entry
kob place pair-limit  --token TETH --quote TUSD --side sell --amount 0.01TETH --price 2500TUSD $N
kob place stop-market --token TETH --quote TUSD --side sell --amount 0.01TETH --stop 2300TUSD $N
kob place ifo         --token TETH --quote TUSD --side buy  --amount 0.01TETH --price 2400TUSD --take-profit 2600TUSD --stop 2300TUSD $N

# amend in place (same covenant id) or cancel-replace; renew a GTC for another 90 days
kob replace <covenant id> --price 0.255KAS $N
kob replace <covenant id> --amount 20TUSD --renew $N
kob replace <covenant id of a buy> --add-kas 10KAS $N
# trailing stops, if-done entries and pair orders are re-planned from their terms with the changes
kob replace <covenant id of a trailing stop> --stop 0.23KAS --trail-gap 0.02KAS $N
kob replace <covenant id of an if-done entry> --price 0.245KAS --take-profit 0.31KAS $N
kob replace <covenant id of a pair order> --price 2450TUSD $N
# cancel, refund an ended order (any key; it keeps the refund tip less the fee), take a resting order
kob cancel <covenant id> <covenant id> $N
kob refund <covenant id> $N
kob take <covenant id> --amount 4TUSD $N
# send
kob send KAS kaspatest:qq... 2.5KAS $N
kob send TUSD kaspatest:qq... 12.5TUSD $N
# merge the key's UTXOs of a token (TOKEN_UTXOS_FRAGMENTED), one confirmation for the chain
kob merge TUSD $N
# views (no signature; --maker KEY|ADDRESS instead of the key)
kob book TUSD --network testnet-10
kob book TETH --quote TUSD --depth 50 --network testnet-10
kob orders --all $N
kob balances --maker kaspatest:qq... --network testnet-10
```

`--json` prints the plan and the result as JSON (the order states, the `createOrder` request, the decoded
transaction, the new covenant ids, the transaction id); the keys, the types and what `--yes` and `--dry-run` change are
under "JSON output" below, the exit codes under "Exit codes".

### Kinds and their options

| kind | required | optional |
|---|---|---|
| `limit` | `--price` | `--tip`, `--min-fill`, `--gtd WHEN` / `--day`, `--active-from WHEN`, `--crossing`, `--max-fills N` (a buy's delivery carriers) |
| `ioc`, `fok` | `--price` | `--tip`, `--min-fill`, `--life DURATION` (1..600 DAA), `--active-from` |
| `market` | | `--tip`, `--min-fill`, `--slippage BPS`, `--all-or-nothing` |
| `stop-market` | `--stop` | `--slippage` (the band), `--min-touch`, `--tip`, `--min-fill`, lifetime, `--max-fills` |
| `stop-limit` | `--stop`, `--limit` | as `stop-market` |
| `take-profit` | `--take-profit` | `--tip`, `--min-fill`, lifetime, `--max-fills` |
| `oco` | `--take-profit`, `--stop` | `--limit`, `--slippage`, `--min-touch`, as `stop-market` |
| `ifd` | `--price` and one of `--take-profit`, `--stop` | `--limit` (exit stop-limit), `--exit-tip`, `--prefund` (sell first), `--tip`, `--min-fill`, lifetime |
| `ifo` | `--price`, `--take-profit`, `--stop` | as `ifd` |
| `streaming` | `--displayed-price`, `--tolerance BPS` | `--tip`, `--min-fill`, `--all-or-nothing` |
| `close` | | `--amount` (default: everything held), `--tip`, `--slippage`, `--all-or-nothing` |
| `twap`, `dca` | `--price`, `--slice AMOUNT`, `--interval DURATION` | `--price-end` and `--slice-auction` (each slice an auction), `--tip`, `--min-fill`, lifetime, `--active-from`; `dca`: `--max-fills` |
| `dutch` | `--price`, `--price-end`, `--duration` | `--step DURATION`, `--tip`, `--min-fill`, lifetime, `--active-from`, `--max-fills` |
| `trailing-stop` | `--stop`, `--trail-step`, `--trail-gap` | `--trail-wait`, `--trail-updates`, `--take-profit`, `--slippage`, as `stop-market` |
| `repeat-ifd`, `repeat-ifo` | as `ifd` (a take-profit) / `ifo` | `--repeat-count K` (default: unlimited within 90 days), as `ifd` |
| `pair-limit` | `--quote`, `--price` (B per whole A) | as `limit` |

Every stop also takes `--band DURATION`, `--keeper-tip KAS` and `--min-rest DURATION`; an if-done entry `--entry-stop`
(a stop entry), `--exit-min-fill`, `--exit-min-touch`, `--exit-slippage` and a trailing exit stop (`--trail-step`,
`--trail-gap`). Every kind takes `--quote TOKEN_B` (a pair order: `KobPair`, `KobCondPair`, `KobIfdPair`). Not here:
taking pair orders and stop legs (the matchers fill them).

## JSON output, exit codes, scripts, token commands

### JSON output (`--json`)

`--json` is a flag of the ten trading commands (`place`, `replace`, `cancel`, `refund`, `take`, `send`, `merge`, `book`,
`orders`, `balances`). `kob token issue|resume|verify-declaration` have no `--json` (text output; the files `--out-dir`
writes are JSON), and `kob order sweep|cancel` and `kob recover` always print a JSON report. In `kob registry`, `programs`,
`add-program` and `verify-genesis` take `--json`.

Conventions of the JSON the CLI prints itself: every amount of KAS or of a token is a **decimal string** in base units
(sompi for KAS, the token's base units for a token; `"2500000"`, negative where it is an outflow, as in `net`), so no 64-bit
value is rounded by a JavaScript parser; covenant ids, transaction ids and keys are lower-case hex strings; counts, indexes,
DAA values (`due`, `lockTime`, `expiry_daa`), `scale` and `signatures` are JSON numbers; flags are booleans; an absent value is
`null`. Strings meant for people (`action`, `inputs`, `outputs`, `price_text`, `what`, `text`, `disclosure`, an issue's
`message`) have no fixed format: do not parse them. The order states and the `createOrder` request that `place` prints are the
protocol library's JSON (`crates/kob-protocol/src/json.rs`: 64-bit integers as decimal strings, byte strings as hex, small
integers as numbers). The shapes below are what the current code prints; this is a pre-release interface (see "Bots and
scripts"). In the examples `<64 hex>` stands for a 64-character hex string.

**The signing commands** (`place`, `replace`, `cancel`, `refund`, `take`, `send`) print one **plan document** on stdout once
a transaction has been built:

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
  "new_covenant_ids": []
}
```

| key | type | content |
|---|---|---|
| `action` | string | a title for people (`place limit sell TUSD`, `amend in place <id>`, `cancel-replace <id>`, `cancel <id>`, `refund <id>`, `take <id> (...)`, `send ... to ...`) |
| `network` | string | `mainnet` or `testnet-10` |
| `plan` | object | the command's own facts, per command (below) |
| `issues` | array | the findings, each `{"code": string, "severity": "error" or "warning" or "info", "message": string}`; an `error` stops the command before signing |
| `transaction.txid` | string | the transaction id |
| `transaction.inputs`, `.outputs` | arrays of strings | one text line per input and output |
| `transaction.fee` | string | the network fee in sompi |
| `transaction.net` | object | the net effect on the key's own wallet: `kas` (string, sompi, negative when KAS leaves, fee included) and `tokens` (an object of covenant id to a signed base-unit string; tokens that net to zero are left out) |
| `transaction.signatures` | number | inputs the key signs |
| `new_covenant_ids` | array of strings | the covenant ids the transaction creates: the new order(s) of a `place`, or of a `replace` that makes a new order; empty for a cancel, refund, take or send |

`plan` per command:

| command | `plan` keys |
|---|---|
| `place` | `states` (array: the order's state and, for a multi-leg kind, its further states), `request` (the web planner's `createOrder` request, or `null`) |
| `replace` | `order` (covenant id), `inPlace` (boolean), `replacement` (the new state), `value` (string, sompi on the replacement); the cancel-replace of a re-planned kind (trailing stop, if-done entry, pair order) adds `states` |
| `cancel` | `action` (`"cancel"`), `covenant_id`, `kind` (the order template's name), `returned` (array of `{"token": id, "units": string, "utxos": number, "kas": string}`: the strays it takes back) |
| `refund` | `order`, `due` (number, DAA), `earned` (string, sompi, negative when the refund costs KAS) |
| `take` | `order`, `amount` (string, base units), `lockTime` (number) |
| `send` of KAS | `to` (as typed), `amount` (string, sompi) |
| `send` of a token | `to`, `token` (covenant id), `amount` (string, base units), `carrier` (string, sompi on the recipient's token output) |

After a successful submission the same command prints a second, **result document** on stdout, right after the plan document:

```json
{
  "status": "submitted",
  "txid": "<64 hex>",
  "fee": "2036",
  "covenant_ids": [],
  "file": null,
  "engine_validated": true
}
```

`status` is always `"submitted"` (a failure prints no result document and exits 1); `covenant_ids` repeats
`new_covenant_ids`; `file` is the path written under `--out-dir`, else `null`; `engine_validated` is always `true` (the
transaction passed the script engine before it was submitted). The two documents are pretty-printed one after the other, not
wrapped in an array: read them as a stream of JSON values (`jq -s` slurps both). `kob cancel A B` runs the ids one after the
other and prints a plan document (and a result document) per id; one id's failure does not stop the next.

**What `--yes` and `--dry-run` change** (the two together are refused, exit 2):

| invocation | stdout | stderr | exit |
|---|---|---|---|
| `--json --dry-run` | the plan document only | `dry run: nothing signed or submitted` | 0 |
| `--json --yes` | the plan document, then the result document | nothing (a warning if the node reports another transaction id) | 0 |
| `--json`, no `--yes`, no terminal | the plan document | `not signed: pass --yes to sign and submit ...` | 1 |
| `--json`, no `--yes`, a terminal | the plan document | the prompt `Sign and submit ... Type yes to continue:`; `yes` or `y` signs, anything else prints `not signed: cancelled` | 0 or 1 |
| a plan with an `error` issue | the plan document | `error: the plan has errors: nothing signed` | 1 |
| a transaction that needs another key's signature | the plan document | `error: the transaction needs signatures of other keys: nothing signed` | 1 |
| nothing could be planned (node or indexer error, not enough KAS, bad terms) | nothing | `error: ...` | 1, or 2 for a usage error (see "Exit codes") |

With `--json` the findings are in `issues` and are not also printed on stderr; without it they are printed on stderr as
`severity: message [CODE]`. The one exception: when `place` cannot build any transaction, it prints the findings on stderr as
text even with `--json`, and stdout stays empty.

**`merge`** prints a summary document and then, unless `--dry-run`, one compact single-line document per submitted link:

```json
{
  "action": "merge TUSD",
  "network": "testnet-10",
  "token": "<64 hex>",
  "utxos_before": 3,
  "utxos_after": 1,
  "transactions": 1,
  "merged_amount": "3500000000",
  "fee": "4500"
}
```

```json
{"link":1,"status":"submitted","txid":"<64 hex>","fee":"4500"}
```

`utxos_before`, `utxos_after` and `transactions` are numbers; `merged_amount` (base units) and `fee` (the whole chain's, sompi;
a link line carries its own) are strings. `--dry-run` prints the summary and stops (exit 0); a link that fails after earlier
links were submitted exits 1 and the message says to run `kob merge` again.

**The views** (`book`, `orders`, `balances`) print one document on stdout and sign nothing; `--yes` and `--dry-run` are
accepted and have no effect. A `book` for a KAS market:

```json
{
  "token": "<64 hex>",
  "quote": null,
  "scale": 100000000,
  "asks": [{ "price": "25000000", "price_text": "0.25 KAS per TUSD", "amount": "500000000", "orders": 2 }],
  "bids": [{ "price": "24500000", "price_text": "0.245 KAS per TUSD", "amount": "300000000", "orders": 1 }],
  "best_ask": "25000000",
  "best_bid": "24500000",
  "last_fill": "24800000"
}
```

* `book`: `token`; `quote` (the covenant id of B for a pair, else `null`); `scale` (number, the order scale; `null` for a token
  without orders and without known decimals); `asks` and `bids` (levels; `price` is the state price as a string, sompi per
  `scale` base units on a KAS book or B base units on a pair; `price_text` the same per whole token; `amount` a string of
  base units; `orders` a number); `best_ask`, `best_bid` and `last_fill` (strings of the state price, `null` when none;
  `last_fill` is always `null` on a pair).
* `orders`: `{"maker": id, "orders": [...]}`; each order has `covenant_id`, `contract` (template name), `market` (ticker or
  `A/B`), `status` (the indexer's), `what` (text), `expiry_daa` (number or `null`), `flags` (array of strings: `expired`,
  `refundable from DAA N`, `possibly frozen`) and `value` (the indexer's `current.value`, passed through as the indexer
  sent it, `null` when absent).
* `balances`: `{"maker": id, "kas": {"free", "locked", "total"}, "tokens": [...], "skipped": [...]}`, the KAS values strings of
  sompi. Each token row has `token`, `ticker`, `decimals` (number or `null`), `free`, `in_orders`, `strays`, `kas_escrow`,
  `kas_carriers`, `kas_locked` (strings, base units or sompi), `free_utxos` (number), `free_verified` (boolean: the free tokens
  were confirmed on the node, else they are the indexer's figure), `orders` (number) and `text` (the same quantities for
  people). `skipped` lists orders not counted: `{"covenant_id", "reason"}`.

**`kob order sweep|cancel` and `kob recover`** print a report on stdout (or write it to `--out FILE`); one line per order and a
summary go to stderr. They sign and submit as soon as they have a key: there is no `--yes` and no prompt (`--dry-run` signs and
engine-validates but never submits).

* `kob order`: `{"action": "sweep" or "cancel", "network", "orders": [...], "rejected": [{"source", "reason"}]}`. Each order has
  `covenant_id`, `kind`, `source` and a `status`: `submitted`, `signed` (a dry run), `skipped` or `refused` (with `reason`), or
  `failed` (with `reason`). A `submitted` or `signed` order also has `txid`, `fee` (string), `funding` (`"order"`, or the outpoint
  of the maker's UTXO that paid the fee), `returned` (as `cancel`'s above), `left_behind` (array of `{"outpoint", "token",
  "reason"}`), `disclosure` (text), `file` (path or `null`) and `engine_validated`.
* `kob recover`: `{"network", "node", "orders": [...], "rejected": [...], "summary": {"live", "not_found", "unsupported",
  "other_maker", "rejected"}}` (numbers in `summary`). Each order has `covenant_id`, `kind`, `template`, `template_hash`, `maker`,
  `status` (`live`, `not_found`, `unsupported` or `other_maker`), `note`, `source` and `candidates`; a live one also `outpoint`,
  `value`, `utxo_daa`, `state`, `amount_left`, `scale`, `min_fill` (strings), `found_by` and `custody`, and with `--cancel` a
  `cancel` object shaped like an order of `kob order` above.

**`kob registry`:** `programs --json` prints the array of the token-program profiles; `add-program --json` prints one document
(`profile`, `detection`, `already_embedded`, `blocked`, `review_tasks`, `registry_template`); `verify-genesis --json` prints its
text lines first and then the array of genesis records (`ticker`, `covenant_id`, `genesis_verified`, and `genesis` or `error`) on
the same stdout, so its output is not one JSON document.

### Exit codes

The code separates three outcomes; the finer causes are told by the message on stderr, never by the number.

| code | meaning |
|---|---|
| 0 | success: the work was done, or `--dry-run` stopped after the plan, or a view was printed, or `--help` / `--version` |
| 1 | the command ran and did not do its work: an error from the node, the indexer or the network check (a node of another network, unreachable), a planning error, a plan with an `error` issue, a transaction that needs another key's signature, signing, script-engine validation or submission failed, **and a refusal to sign: no `--yes` without a terminal, or an answer at the prompt other than `yes`** |
| 2 | usage: an unknown command or option, a missing or malformed argument, no `--network`, no signing key (signing commands), a key on the command line (`--key` of a trading command), `--yes` together with `--dry-run`, `kob place` without a kind; for `kob place` also an invalid order term (an amount without a unit, a required option of the kind missing) |

So the code does not distinguish a refusal (no `--yes`), a plan error, a node error and a user who declined: all are 1. Read
stderr (`error: ...` for failures, `not signed: ...` for a refusal or a declined prompt) or, with `--json`, the `issues` array
of the plan document. A bad term is 2 only where it is read before planning: `place` reports its option errors as 2, while
`replace`, `take`, `send` and the views report a term they cannot read (for example a bare `2.5` for an amount) while planning,
as 1. `kob` with no command, `kob help` and `--help` print the usage on stdout and exit 0; an unknown command (also `kob token
--help` and `kob registry --help`) prints the usage on stderr and exits 2; `kob place`, `kob order` and `kob token resume` with no
arguments print their usage and exit 2.

Per family:

* Trading commands: `kob cancel A B` tries every id; the exit code is that of the last id that failed (0 when none did).
* `kob order sweep|cancel`, `kob recover`: 0 when every attempted order was handled, 1 when an order is `failed` or `refused`
  (for `kob order` also when a file was `rejected`), when the report could not be written, or on an error such as an unreadable
  file or an unreachable node; 2 for an argument error. An order that is only `skipped` does not make the code 1. `kob recover`
  without `--cancel` is 0 whatever it found.
* `kob token issue`: 0 on success (or a dry run) and 1 on any error, a bad option included; `resume`: 0 or 1, and 2 with no
  argument; `verify-declaration`: 0 when the proof verifies, 1 when it does not or cannot be read, 2 for a wrong number of arguments.
* `kob registry validate`: 0 valid, 1 invalid, 2 usage or an unreadable file; `verify-genesis`: 0 when the registry says exactly
  what the evidence proves, 1 on a mismatch or an invalid registry or evidence file, 2 for usage or an unreadable file;
  `programs`: 0, or 2 for usage; `add-program`: 0, 1 when the profile cannot be derived or `--write` is refused, 2 for usage.

### Bots and scripts

The CLI is the supported way for a program that is not written in Rust to place, amend, cancel or read orders: run it as a child
process with `--json --yes`, give the key with `--key-file` or `$KOB_KEY` (never on the command line), read the JSON from stdout
and use the exit code as the success flag (stderr is the reason). Check the exit code before parsing stdout: when no transaction
could be built, stdout is empty. Run `--dry-run --json` first when a script wants to look at the plan before it signs.

`crates/kob-wasm` (the wasm bindings) and `web/src/kob` (the TypeScript planner) are internal to the web app: they carry no
stability promise, and no document of the repository offers them to third-party programs as an interface. The repository README
says that "interfaces, encodings and artifact ids can still change"; the same holds for the CLI's option names and the JSON
shapes above until a release says otherwise.

### Token commands

`kob token issue` creates a fixed-supply KCC-20 token in two transactions (KCC-23): a **preparation** that spends the funding
into one output of the funding key and declares the token's metadata in its payload, then the **genesis** that spends that
output, so the covenant id commits to the declaration. Both are signed at once and the genesis is sent right after the
preparation. Required: `--name`, `--ticker` (2 to 12 of A-Z 0-9), `--decimals` (0 to 18), at least one `--fund-utxo
TXID:INDEX:AMOUNT:OWNER` (sompi; all of one key; OWNER is a 64-hex x-only key or a P2PK address) and `--supply` and/or at least one
`--holder OWNER:SCHEME:AMOUNT[:BORROW_SCHEME:GUARD]` (without `--holder` the whole supply goes to the funding key). Optional:
`--program 3x3|public-mint|8x8-prototype|kasparocket-testnet` (default `3x3`), `--carrier`, `--fee`, `--fee-rate`, `--change-to`,
`--extension-commitment`, `--allow-borrow`, `--description`, `--logo` (or `--icon`), `--website`, `--x`, `--telegram`, `--out-dir`,
`--network`, `--node`, `--dry-run` and `--key HEX|env:VAR|file:PATH`. `kob token issue --help` lists them all.

* **Dry run.** `--dry-run` builds and verifies everything offline and never submits. With `--key` it signs and executes every
  input in the script engine; without it the transactions stay unsigned and script execution is skipped (the output says so).
  Nothing is written unless `--out-dir` is given.
* **Submitting.** Without `--dry-run`, `--key` and `--node` (or `$KOB_NODE_WRPC`) are required, and the command submits at once:
  there is no `--yes` and no confirmation prompt. The files (`preparation_tx.json`, `genesis_tx.json`, `submit_preparation.json`,
  `submit_request.json`, `supply.json`, `metadata.json`, `metadata.payload.hex`, `launch_proof.json`, `registry_entry.json`,
  `creation.json`) go to `--out-dir`, else to `./kob-create-<first 16 hex of the covenant id>`. The output is text, there is no
  `--json`.
* `--key` of these token commands, of `kob recover` and of `kob order` is **not** refused on the command line: `HEX` is accepted
  there (visible in `ps` and the shell history); use `env:NAME` or `file:PATH`.

```sh
kob token issue --name "Test USD" --ticker TUSD --decimals 6 --supply 1000000000000 \
  --fund-utxo <64 hex txid>:0:5000000000:kaspatest:qq... \
  --network testnet-10 --key file:$HOME/.kob/maker.key --out-dir ./tusd-creation --dry-run
```

The same command without `--dry-run` and with `--node ws://127.0.0.1:18210` submits the preparation and the genesis. The
supply is in base units (here 1,000,000 TUSD at 6 decimals), all to the funding key.

`kob token resume <DIR> [--node ws://HOST:PORT]` sends a creation whose genesis did not go out. `DIR` is the directory
`kob token issue` wrote (`creation.json`, `preparation_tx.json`, `genesis_tx.json`). Nothing is signed again: the saved, signed
transactions are sent as they are, the preparation first (a node that already has it refuses it, which is tolerated), then the
genesis. It refuses transactions that are not signed (a dry run without `--key`) or a genesis that does not spend output 0 of the
preparation. When the genesis fails, `kob token issue` prints this command as the way on.

`kob token verify-declaration <launch_proof.json>` checks a KCC-23 launch proof that may come from anyone (an indexer's
`GET /v1/tokens/<id>/declaration`, a community list, `kob token issue`): it recomputes both transaction ids and the covenant id
and prints the covenant id, the transaction ids, the declaration and its name, symbol and decimals only when the declaration is
bound to that covenant id (exit 0); otherwise it prints the error on stderr and exits 1.

`kob token` has no help of its own: `kob token --help` (and `kob token` alone) is an unknown command, prints the top-level usage
on stderr and exits 2. Use `kob token issue --help`, `kob token resume --help` and `kob token verify-declaration --help`; likewise
`kob registry --help` is not a command (`kob registry verify-genesis --help` and `kob registry add-program --help` are).
