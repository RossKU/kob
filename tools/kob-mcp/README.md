# kob-mcp

An MCP server that lets an AI agent (Claude Desktop, Claude Code) read KOB order books and trade on them through the
[`kob` CLI](../../crates/kob-cli/README.md). The key stays in a local file; the server never reads it. Testnet-10 by default.

> KOB has had no external audit. Use a wallet that holds only what the agent may spend.

## Install

Claude Desktop (Node 22 or later):

```sh
cd tools/kob-mcp && npm ci --omit=dev
npx @anthropic-ai/mcpb@2.1.2 pack . kob-mcp.mcpb   # open the .mcpb with Claude Desktop
```

The install form asks for the `kob` binary, the key file, your primary wallet, the caps and a data folder.

Claude Code:

```sh
claude mcp add kob -- node /path/to/kob/tools/kob-mcp/server/index.mjs --config /path/to/kob-mcp.json
```

Config: [config.example.json](config.example.json) (defaults in [src/config.mjs](src/config.mjs)).

## Tools

- Read: `kob_status`, `kob_tokens`, `kob_book`, `kob_stats`, `kob_candles`, `kob_balances`, `kob_orders`, `kob_order`,
  `kob_fee_estimate`.
- Write: `kob_place_limit`, `kob_replace`, `kob_cancel`, `kob_take` (price bound required), `kob_merge`, `kob_claim`,
  `kob_token_create`, `kob_send`.

A write first returns its dry-run plan and a `planId`; it is sent only when the same call comes back with
`confirm: "<planId>"` within 5 minutes (or at once in `"mode": "auto"`, still within the limits).

## Safeguards

- Caps on the KAS that leaves the wallet: per transaction, per UTC day, and the fee per transaction.
- At most 3 token creations a day (`launchesPerDay`).
- A price more than 25 % worse than the last fill is refused.
- `kob_send` pays only your own addresses (`withdrawTo`).
- A `STOP` file in the data folder blocks every write.
- Every call is logged in `audit.jsonl`.
- Mainnet needs `"network": "mainnet"` and `"allowMainnet": true`.

For an agent without a chat, see [tools/mm-bot](../mm-bot/README.md). Tests: `npm test`; a live testnet-10 run:
[TN10-RUN.md](TN10-RUN.md).
