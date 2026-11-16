# kob-mcp

An MCP server that lets an AI agent (Claude Desktop, Claude Code) read KOB order books and trade on them. It drives the
[`kob` CLI](../../crates/kob-cli/README.md) as a child process, as [tools/mm-bot](../mm-bot/README.md) does: `--json`,
`--dry-run` before `--yes`, the exit code read first, the key only as `--key-file` (or `KOB_KEY`). The server never reads
the key, and no tool shows its path. It runs on testnet-10 by default.

> KOB has had no external audit. Use a wallet that holds only what the agent may spend, and try it on testnet-10 first.

## Tools

| Read (sign nothing) | |
|---|---|
| `kob_status` | wallet address, network, mode, limits, today's spend, kill switch |
| `kob_tokens` | tokens with a market |
| `kob_book` | a book as price levels, or (`orders: true`) the single orders with their ids |
| `kob_stats`, `kob_candles` | 24 h stats; OHLCV candles (indexer) |
| `kob_balances`, `kob_orders`, `kob_order` | the wallet's KAS, tokens and orders; one order's details and events |
| `kob_fee_estimate` | a dry run of any write: fee, KAS out of the wallet, whether the limits pass |

| Write | |
|---|---|
| `kob_place_limit` | a limit buy or sell (`delivery`, `tip`, `gtd`, `day`, `crossing` optional) |
| `kob_replace`, `kob_cancel` | amend or cancel an order |
| `kob_take` | fill a resting order; `max_price` (buy) or `min_price` (sell) is required |
| `kob_merge` | merge a token's UTXOs |
| `kob_claim` | claim a collected buy order's tokens (`merge`, `sell_price`, `end`) |
| `kob_token_create` | a fixed-supply KCC-20 token, the whole supply to this wallet |
| `kob_send` | KAS or a token, only to an allow-listed address |

At start-up the server reads `kob claim --help` and `kob place --help`: `kob_claim` and the `delivery` argument are
registered only when the CLI offers them, and `kob_status` says which (`cli`).

## How a write goes

1. The call without `confirm` runs the CLI with `--dry-run` and returns the plan: the action, the inputs and outputs,
   the fee, the KAS that leaves the wallet (`spendKas`: escrow, carriers, sends and fee) and a `planId`. Nothing is signed.
2. The same call with `confirm: "<planId>"` within `planTtlSec` (default 300 s) plans again, checks again and runs `--yes`.
   Other arguments, a used or an expired plan id, or a new plan that takes more than the confirmed one plus `maxFeeKas`
   are refused.
   The sent transaction is planned again, so its ids can differ from the dry run's; what must match is the KAS it takes.
3. With `"mode": "auto"` the first call checks and sends at once. The limits still apply.

## Safeguards

- **Network.** testnet-10 unless the config says `"network": "mainnet"` and also `"allowMainnet": true`.
- **Key.** Only its path is configured; the CLI reads the file. The path is removed from every result and log line.
- **Caps** (KAS that leaves the wallet, `max(fee, -net)` of the plan): `perTxKas` per transaction, `perDayKas` per UTC day,
  `maxFeeKas` per transaction. The day is summed from `ledger.jsonl` in the data folder: a send is reserved before `--yes`
  and settled from the CLI's result, released only when nothing was sent, and counted when the outcome is unknown.
  The caps count gross outflow: KAS a cancel or a fill brings back does not free any of the day's cap.
- **Launches.** At most `launchesPerDay` (default 3) token creations per UTC day.
- **Price band.** A limit price or take bound more than `priceBandPct` (default 25 %) worse than the last fill (else the
  mid) is refused, in exact integers at any number of decimals. It catches mistakes, not a market someone moves. The
  caps are the hard limit.
- **Take.** The CLI's bound refusal is read in both forms: `TAKE_PRICE_ABOVE_MAX` / `TAKE_PRICE_BELOW_MIN` in the JSON
  `issues`, or only the stderr text when the fill could not be built either.
- **Withdrawals.** `kob_send` pays only the addresses in `withdrawTo` (your primary wallet). No tool takes another
  recipient: token creation sends the supply and the change to this wallet.
- **Kill switch.** While `<dataDir>/STOP` (or `killSwitchFile`) exists, every write is refused before the CLI runs. Reads
  still work.
- **Audit log.** `audit.jsonl` has one line per call: arguments, outcome, plan id, transaction ids, spend.
- Arguments are checked before they reach the CLI (no shell; units required; nothing that starts with `-`). Token
  names and descriptions from the indexer are third-party data.

Keep one data folder per wallet: the caps count what this folder's ledger saw.

## Claude Desktop

It needs Node 22 or later (global `WebSocket` and `fetch`), as Claude Desktop's bundled Node or your own; the
manifest says `node >=22.0.0` and the server refuses an older one. Build the extension (`.mcpb`) and open it with Claude Desktop:

```sh
cd tools/kob-mcp && npm ci --omit=dev
npx @anthropic-ai/mcpb@2.1.2 pack . kob-mcp.mcpb
```

The install form asks for the `kob` binary, the key file, the primary wallet, the three caps, the launches per day and the data folder; a node
URL (needed to create tokens) and a config file for the other settings are optional. The form's values win over the file.

## Claude Code

```sh
claude mcp add kob -- node /path/to/kob/tools/kob-mcp/server/index.mjs --config /path/to/kob-mcp.json
```

or in `.mcp.json`:

```json
{ "mcpServers": { "kob": { "command": "node", "args": ["/path/to/kob/tools/kob-mcp/server/index.mjs", "--config", "/path/to/kob-mcp.json"] } } }
```

## Config

[config.example.json](config.example.json); relative paths are relative to the file. Defaults are in
[src/config.mjs](src/config.mjs). `node` must be a JSON wRPC URL (`ws://host:18210`) for `kob_token_create` to find a
funding UTXO, or pass `fund_utxo`. The `KOB_MCP_*` variables of [manifest.json](manifest.json) override the file.

## An agent without a chat

A fully automated agent is [tools/mm-bot](../mm-bot/README.md): a loop that quotes one token through the same CLI, with
its own caps, STOP file and keepalive.

## Tests

`npm test`: the caps, the ledger, the allow-list, confirm-before-send and the kill switch against a stand-in CLI, and an
MCP client that starts the server over stdio. A live testnet-10 run: [TN10-RUN.md](TN10-RUN.md).
