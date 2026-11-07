# kob (kob-cli)

`kob` is the KOB command line: read order books, trade, create tokens and follow the live feed. It builds the same
transactions as the web wallet.

> **Open review.** KOB has had no external audit. Interfaces, option names and JSON shapes can still change. Try things on
> testnet-10 first, and run every signing command with `--dry-run` before you sign.

Details (planner defaults, units, every option, the `--json` shapes, all exit codes): [REFERENCE.md](REFERENCE.md).
Options of one command: `kob <command> --help`.

## Install

Download `kob-<version>-<network>-<target>` from the [releases](https://github.com/RossKU/kob/releases) (`.zip` on Windows,
`.tar.gz` elsewhere), check it against the release's `SHA256SUMS`, and unpack it. Or build it:

```sh
cargo build --release --locked --bin kob    # target/release/kob (kob.exe on Windows)
kob --version
```

## Quick start

```sh
# 1. Read a book (no key). Testnet-10 has a default indexer; mainnet needs --indexer for now.
kob book TUSD --testnet

# 2. Plan an order and stop before signing (pick a price from the book; 0.25KAS is only an example)
kob place limit --token TUSD --side sell --amount 12.5TUSD --price 0.25KAS \
  --testnet --key-file ~/.kob/maker.key --dry-run

# 3. The same order, signed and submitted (or leave out --yes and answer the prompt)
kob place limit --token TUSD --side sell --amount 12.5TUSD --price 0.25KAS \
  --testnet --key-file ~/.kob/maker.key --yes

# 4. List your orders and cancel one
kob orders --testnet --key-file ~/.kob/maker.key
kob cancel <covenant id> --testnet --key-file ~/.kob/maker.key
```

Every signing command prints the plan and the decoded transaction first. It signs only with `--yes` or after you type
`yes` at a terminal; without a terminal and without `--yes` it refuses. `--dry-run` cannot be combined with `--yes`.

## Configuration

| Setting | Flag | Environment | Default |
|---|---|---|---|
| Network | `--network mainnet\|testnet-10`, `--testnet`, `--netsuffix=10` | | mainnet (a deployment build is bound to its own network) |
| Node (wRPC) | `--node URL` | `KOB_NODE_WRPC` | the public nodes named by the Kaspa resolvers (two hosts that must agree) |
| Indexer | `--indexer URL` (`http://` or `https://`) | `KOB_INDEXER_URL` | testnet-10: `https://kob-api.kaspa.ag`; mainnet: none yet |
| Signing key | `--key-file PATH`, `--key-env NAME` | `KOB_KEY` | none |
| Token registry | `--registry FILE` | | the registry embedded for the network |
| Fee | `--fee-rate N`, `--priority-fee` | | 100 sompi per gram, relay minimum |
| Local state, cache | | `KOB_STATE_DIR`, `KOB_CACHE_DIR` | per-user state and cache directories |

- **Network.** Every plan and prompt names the network. The node must run it, or nothing is planned or signed.
  The default indexer comes from `contracts/deploy/<network>/deployment.json` (`indexer_url`). Builds from before the web app moved to
  Cloudflare Pages default to `https://kob.kaspa.ag/indexer` on testnet-10, which is gone: such a build fails. Pass
  `--indexer https://kob-api.kaspa.ag` or set `KOB_INDEXER_URL=https://kob-api.kaspa.ag`, or install a current build.
- **Own node.** JSON wRPC `ws://host:18110` (mainnet) or `:18210` (testnet-10); Borsh `ws://host:17110` / `:17210`. Put a URL
  with a password in `KOB_NODE_WRPC`, not on the command line. `KOB_RESOLVERS` replaces the resolver list.
- **Fees.** `--priority-fee` also pays for the storage mass (the web app's "Pay the priority fee"). Use it when blocks are
  contended; the CLI prints a `note:` on stderr when they are. See [Fees](REFERENCE.md#fees).

### Keys

- The key is a file holding the 64-hex secret (`--key-file`), or an environment variable (`KOB_KEY`, or `--key-env NAME`).
  `--key-file` wins over the variable.
- Trading commands refuse `--key` and any argument that contains the key: arguments are visible to other users (`ps`, shell
  history). The key is never printed.
- The CLI does not check the file's permissions. Protect it yourself: `chmod 600` on Unix, an owner-only ACL on Windows.
- `kob token create`, `kob recover` and `kob order` take `--key HEX|env:NAME|file:PATH` instead. `HEX` works there but is
  visible to others; use `env:` or `file:`.
- Signing commands: `place`, `replace`, `cancel`, `refund`, `take`, `send`, `merge`, `token create`, `recover --cancel`,
  `order sweep|cancel`. `orders` and `balances` use only the public key (or `--maker KEY|ADDRESS`); the rest need no key.

## Commands

| Command | What it does |
|---|---|
| `kob place <KIND>` | place an order (kinds below) |
| `kob replace <ID>` | amend a live order in place, or cancel-replace it in one transaction |
| `kob cancel <ID>...` | the maker's cancel: custody, strays and KAS back to the maker |
| `kob refund <ID>` | refund an ended order; any key may send it and keeps the refund tip, less the fee |
| `kob take <ID> --amount A` | fill a resting order as the taker |
| `kob send <TOKEN\|KAS> <TO> <AMOUNT>` | send a token or KAS |
| `kob merge <TOKEN>` | merge your UTXOs of one token into one |
| `kob book <TOKEN> [--quote B] [--depth N]` | the order book on KAS or of a pair (depth default 20, max 500) |
| `kob orders [--token T] [--all]` | your live orders (`--all`: every status) |
| `kob balances` | free KAS and tokens, and what your live orders hold |
| `kob tokens [SEARCH] [--all]` | tokens with a KOB market; `--all` adds kascov.io's list |
| `kob watch fills\|book\|order\|health` | follow the live feed (read-only) |
| `kob order sweep\|cancel --view FILE` | sweep strays or cancel from saved indexer views |
| `kob recover --from FILE` | find your orders on a node from a backup or export, and cancel them (`--cancel`) |
| `kob token create` | create a fixed-supply KCC-20 token |
| `kob token resume <DIR>` | send a creation whose genesis did not go out |
| `kob token verify-declaration <PROOF>` | check a KCC-23 launch proof |
| `kob registry validate\|verify-genesis\|programs\|add-program` | token registry and token-program tools |

`<ID>` is an order's covenant id (64 hex). `<TOKEN>` is a ticker or a covenant id. A ticker that names more than one
token is refused: give the covenant id.

## Units

Every amount needs a unit; a bare `2.5` is refused. Nothing is rounded.

| What | Units | Example |
|---|---|---|
| token amount | ticker or `tok` (whole tokens), `base` (base units) | `12.5TUSD`, `1250000000base` |
| KAS amount | `KAS`, `sompi` | `2.5KAS`, `200000000sompi` |
| price | KAS per whole token; on a pair, B per whole A | `0.25KAS`, `2500TUSD` |
| tip | KAS per whole token, on top of the price | `0.001KAS` |
| percentage | `%` or `bps` | `3%`, `300bps` |
| duration | `s`, `m`, `h`, `d`, `daa` | `30s`, `600daa` |
| time | UTC `YYYY-MM-DDTHH:MM[:SS]Z` or `daa:N` | `2026-10-20T00:00Z` |

Prices are all-in: a sell receives `price - tip`, a buy pays `price + tip`.

## Order kinds

`kob place <KIND> --token T --side buy|sell --amount A [options]`. Add `--quote B` to any kind for a pair order.

| Kind | Required | What it is |
|---|---|---|
| `limit` | `--price` | GTC (90 days idle), or `--gtd WHEN` / `--day`; `--active-from` delays it |
| `ioc`, `fok` | `--price` | immediate-or-cancel / fill-or-kill (`--life`, default 300 DAA) |
| `market` | | an auction from the best price to a 3 % bound (`--slippage`) |
| `streaming` | `--displayed-price`, `--tolerance` | an auction from the price you saw, within your tolerance |
| `close` | | sell everything held (or `--amount`), as a market auction |
| `twap`, `dca` | `--price`, `--slice`, `--interval` | sell (twap) or buy (dca) in slices; `--side` not needed |
| `dutch` | `--price`, `--price-end`, `--duration` | a decaying ask or a rising bid |
| `stop-market` | `--stop` | a market order when the price touches the stop |
| `stop-limit` | `--stop`, `--limit` | a limit order when the price touches the stop |
| `trailing-stop` | `--stop`, `--trail-step`, `--trail-gap` | a stop that follows the market |
| `take-profit` | `--take-profit` | an exit at a better price |
| `oco` | `--take-profit`, `--stop` | take-profit and stop, one cancels the other |
| `ifd` | `--price` and `--take-profit` or `--stop` | if-done: an entry, then its exit |
| `ifo` | `--price`, `--take-profit`, `--stop` | if-done with both exits |
| `repeat-ifd`, `repeat-ifo` | as `ifd` / `ifo` | re-enters after every take-profit (`--repeat-count K`) |
| `pair-limit` | `--quote`, `--price` | a limit on a pair (price in B per whole A) |

Defaults match the web wallet: minimum fill worth 10 KAS, 1 KAS carrier per covenant UTXO, tip 0. Every option per kind:
[REFERENCE.md](REFERENCE.md#order-kinds-every-option).

**Market sells need an extra flag.** The auction starts at the indexer's best price, which nothing independent checks. A
`market` or `close` sell needs `--ack-market-start` on its own command line (`--yes` is not enough), or `--floor-price P`.

## Examples

```sh
export KOB_NODE_WRPC=ws://127.0.0.1:18210 KOB_INDEXER_URL=http://127.0.0.1:8090   # optional: your own node and indexer
N="--network testnet-10 --key-file ~/.kob/maker.key"

# limit orders: a buy with a tip and an end date; a sell that starts later; a day order
kob place limit --token TUSD --side buy  --amount 100TUSD --price 0.24KAS --tip 0.0001KAS --gtd 2026-12-31T00:00Z $N
kob place limit --token TUSD --side sell --amount 5TUSD   --price 0.26KAS --active-from 2026-12-01T09:00Z $N
kob place limit --token TUSD --side sell --amount 5TUSD   --price 0.26KAS --day $N
# immediate orders
kob place fok    --token TUSD --side sell --amount 40TUSD --price 0.24KAS --life 30s $N
kob place market --token TUSD --side sell --amount 40TUSD --slippage 1% --floor-price 0.23KAS $N
# stops and exits
kob place stop-limit --token TUSD --side sell --amount 40TUSD --stop 0.22KAS --limit 0.215KAS $N
kob place oco        --token TUSD --side sell --amount 40TUSD --take-profit 0.30KAS --stop 0.22KAS $N
kob place ifo        --token TUSD --side buy  --amount 40TUSD --price 0.25KAS --take-profit 0.30KAS --stop 0.22KAS $N
# time-based: 5 TUSD every hour; a Dutch sell from 0.30 to 0.25 over 2 h
kob place dca   --token TUSD --amount 40TUSD --slice 5TUSD --interval 1h --price 0.24KAS $N
kob place dutch --token TUSD --side sell --amount 40TUSD --price 0.30KAS --price-end 0.25KAS --duration 2h $N
# a pair order: TETH priced in TUSD
kob place pair-limit --token TETH --quote TUSD --side sell --amount 0.01TETH --price 2500TUSD $N

# amend, renew, cancel, refund, take
kob replace <covenant id> --price 0.255KAS $N
kob replace <covenant id> --amount 20TUSD --renew $N
kob cancel <covenant id> <covenant id> $N
kob refund <covenant id> $N
kob take <covenant id> --amount 4TUSD $N
# send and merge
kob send KAS  kaspatest:qq... 2.5KAS $N
kob send TUSD kaspatest:qq... 12.5TUSD $N
kob merge TUSD $N
# views (no signature; --maker instead of the key)
kob book TETH --quote TUSD --depth 50 --testnet
kob balances --maker kaspatest:qq... --testnet
```

## Watching the live feed

`kob watch` follows the executor's WebSocket and prints one line per event. It needs no key and signs nothing.

```sh
kob watch fills [--token TOKEN]                 # every fill, or one token's
kob watch book TOKEN [--quote B] [--depth N]    # the book, printed again when it may have changed
kob watch order COVENANT_ID...                  # each order's state, printed again when it changes
kob watch health                                # indexer progress; a health line every 15 s
```

- The address comes from the indexer: `https://host/path` becomes `wss://host/path/v1/ws`.
- It reconnects by itself (1 s up to 30 s). Fills during a gap are not replayed. Ctrl-C ends it (exit 0).
- `--json` prints NDJSON: one JSON document per line. Events go to stdout; notes and errors to stderr.

## JSON output and exit codes

- `--json` makes a command print **one JSON document** on stdout: the plan, the decoded transaction, the new covenant ids,
  and the submission in its `result` key (`null` until submitted).
- Amounts are decimal strings in base units or sompi; ids are lower-case hex. Do not parse text meant for people.
  `kob cancel A B` prints one document per id; `kob watch --json` one per line.

| Exit | Meaning |
|---|---|
| 0 | done, `--dry-run` stopped after the plan, or a view was printed |
| 1 | the command ran but did not do its work: node / indexer / plan error, or a refusal to sign (no `--yes`, or a declined prompt) |
| 2 | usage (trading commands): unknown option, malformed argument (e.g. an amount without a unit), no key, `--key` on a trading command, `--yes` with `--dry-run` |

The reason is on stderr (`error: ...` or `not signed: ...`) and, with `--json`, in the `issues` array. Other commands
differ a little (`kob token create` uses 1 for every error): see [exit codes](REFERENCE.md#exit-codes) and every
[JSON key and type](REFERENCE.md#json-output).

## Bots and scripts

The CLI is the supported way for a program not written in Rust to place, amend, cancel or read orders:

1. Run `kob ... --json --dry-run` and read the plan.
2. Run `kob ... --json --yes` as a child process. Give the key with `--key-file` or `KOB_KEY`, never on the command line.
3. Check the exit code first: when nothing could be planned, stdout is empty and the reason is on stderr.
4. Then parse stdout. `result.status` is `"submitted"` and `result.txid` is the transaction id.

`crates/kob-wasm` and `web/src/kob` are internal to the web app and have no stability promise. The CLI's options and JSON
are a pre-release interface too.

## Token commands

`kob token create` creates a fixed-supply KCC-20 token in two transactions: a preparation that declares the metadata
(KCC-23), then the genesis.

```sh
kob token create --name "Test USD" --ticker TUSD --decimals 6 --supply 1000000000000 \
  --fund-utxo <64 hex txid>:0:5000000000:kaspatest:qq... \
  --network testnet-10 --key file:$HOME/.kob/maker.key --out-dir ./tusd-creation --dry-run
```

- Required: `--name`, `--ticker` (2 to 12 of A-Z 0-9), `--decimals` (0 to 18), at least one `--fund-utxo` (amount in sompi),
  and `--supply` and/or `--holder`. The supply is in base units (here 1,000,000 TUSD), all to the funding key by default.
- Drop `--dry-run` and add `--yes` to submit (the public nodes are used unless `--node` or `KOB_NODE_WRPC` names one).
- `--program public-mint` (default) is what kascov.io lists as KCC-20; `--program 3x3` is cheaper per transfer but other
  indexers do not recognise it.
- Files go to `--out-dir` (a submitting run without it: `./kob-create-<id>`). If the genesis did not go out:
  `kob token resume <DIR>`.
- `kob token verify-declaration <launch_proof.json>` checks a launch proof from any source.
- Help: `kob token create --help` (`kob token --help` is not a command). More: [details](REFERENCE.md#token-commands-details).

## Finding tokens

`kob tokens [SEARCH] [--all] [--refresh] [--json]` lists tokens. It needs no key and no node.

- By default: every token the indexer knows (an order was placed, or the registry lists it), with covenant id, decimals,
  program, open asks and bids, and the indexer's warnings (such as `live_mint_authority` or `freeze`).
- `--all` adds every token kascov.io lists for the network. Their tickers and decimals are the tokens' own claims.
- `SEARCH` is a ticker prefix (any case) or a covenant-id prefix.
- KOB makes no judgement of a token: there is no "verified" or "official" column. Tickers collide; the covenant id is the
  identity. More: [details](REFERENCE.md#kob-tokens-details).
