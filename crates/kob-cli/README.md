# kob (kob-cli)

The KOB command line. `kob --help` lists every command, `kob <command> --help` its options.

| Command | What it does |
|---|---|
| `kob place <KIND>` | place an order, planned exactly like the web wallet's order ticket |
| `kob replace <ID>` | amend a live order in place, or cancel-replace it in one transaction |
| `kob cancel <ID>...` | the maker's cancel by covenant id (custody, strays and KAS back to the maker) |
| `kob refund <ID>` | the permissionless refund of an order that has ended (the sender keeps the refund tip) |
| `kob take <ID> --amount A` | fill a resting order as the taker |
| `kob send <TOKEN\|KAS> <TO> <AMOUNT>` | send a token (within its program's slots) or KAS |
| `kob order sweep\|cancel --view FILE` | the maker's stray sweep in place and cancel from saved indexer views |
| `kob recover --from FILE` | find the maker's orders from a backup / export / view on a node alone, and cancel them |
| `kob token issue` | issue a fixed-supply KCC-20 token |
| `kob registry validate\|verify-genesis\|programs\|add-program` | the token registry and token-program profiles |

## The trading commands

`place`, `replace`, `cancel`, `refund`, `take` and `send` read the node (wRPC JSON, `--node` or `$KOB_NODE_WRPC`) and the
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
a market start checked against the last fill (`--ack-market-start`), self-trade and book checks that need the indexer
(`--ack-no-guards` when it cannot answer). Every UTXO the indexer names is looked up on the node before it is spent.
`crates/kob-cli/src/trade/tests_plan.rs` replays the web planner's test cases and the golden vectors those tests compare
with (`crates/kob-protocol/vectors/golden.json`).

**Prices are all-in.** A price is the order's limit per whole token; the tip comes on top of it: a sell receives
`price - tip`, a buy pays `price + tip`. The plan shows the all-in price and the all-in total of the whole amount by the
covenant's rounding (a sell receives at least the ceiling, a buy pays at most the floor).

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

**Safety.**

* The signing key comes from `--key-file PATH` (a file holding the 64-hex secret) or the environment (`$KOB_KEY`, or
  `--key-env NAME`). `--key` is refused, and so is any argument containing the key: arguments are visible to other
  users (`ps`, shell history). The key is never printed.
* Every command prints the plan (the order's terms, carriers, fee, expiry) and the decoded transaction (each input and
  output with its owner, the net effect on the wallet in KAS and each token, the fee, the signatures asked for) before
  anything is signed. `--dry-run` stops there. Without `--yes` a terminal is asked (`yes` to continue); without a
  terminal the command refuses. A transaction that needs another key's signature is never signed.
* Mainnet is refused unless named: `--network mainnet`, even by a mainnet deployment build.
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
# a pair limit: TETH for TUSD at 2500 TUSD per TETH
kob place pair-limit --token TETH --quote TUSD --side sell --amount 0.01TETH --price 2500TUSD $N

# amend in place (same covenant id) or cancel-replace; renew a GTC for another 90 days
kob replace <covenant id> --price 0.255KAS $N
kob replace <covenant id> --amount 20TUSD --renew $N
kob replace <covenant id of a buy> --add-kas 10KAS $N
# cancel, refund an ended order (any key; it keeps the refund tip less the fee), take a resting order
kob cancel <covenant id> <covenant id> $N
kob refund <covenant id> $N
kob take <covenant id> --amount 4TUSD $N
# send
kob send KAS kaspatest:qq... 2.5KAS $N
kob send TUSD kaspatest:qq... 12.5TUSD $N
```

`--json` prints the plan and the result as JSON (the order states, the `createOrder` request, the decoded
transaction, the new covenant ids, the transaction id).

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
| `pair-limit` | `--quote`, `--price` (B per whole A) | as `limit` |

Not here yet: trailing stops, TWAP / DCA, Dutch / rising bids, streaming and close orders, repeat IFD / IFO, stop
entries, and the pair conditional and if-done kinds (the web wallet places them); taking pair orders and stop legs (the
matchers fill them).
