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
- Write: `kob_place_limit`, `kob_replace`, `kob_cancel`, `kob_take` (token id and price bound required), `kob_merge`, `kob_claim`,
  `kob_token_create` (`sell: "curve"` puts the new supply on a sell curve in the genesis, `keep_percent` of it kept; `lock_until: "+90d"` locks the kept share on chain until then, without a sale `keep_percent` of the supply, default all), `kob_send`.

A write first returns its dry-run plan and a `planId`; it is sent only when the same call comes back with
`confirm: "<planId>"` within 5 minutes (or at once in `"mode": "auto"`, still within the limits). The agent itself sends
the confirm: the plan step is a check of the dry run, not a human approval, so a steered agent can confirm its own plan.
The limits below are what holds.

## Safeguards

- Caps on the KAS that leaves the wallet: per transaction, per UTC day, and the fee per transaction. A send whose outcome
  is unknown (a lost reply, or a node that says it rejected the submit) counts until the day rolls; `kob_status` lists
  it with its txid.
- Caps on the tokens that leave the wallet (sends, sells, sales into buy orders): `limits.tokens` per covenant id in base
  units, else 25 % per transaction and 50 % per day of the holding confirmed on the node (`limits.tokenDefault`).
- At most 3 token creations a day (`launchesPerDay`).
- A price (all-in: a buy's price plus its tip, a sell's price less its tip) more than 25 % worse than the reference is
  refused. A buy is judged against the lower of the last fill and the 10 KAS bid depth (the bid behind 10 KAS of bids,
  midpointed with the best ask), a sell against the higher, so one printed fill moves neither bound. A book that cannot
  be read refuses.
- The tip goes to the executor that fills the order: at most 1 % of the price and 1 KAS over the whole order
  (`limits.maxTipPct`, `limits.maxTipKas`).
- `kob_replace` reads the order's side and token from the CLI's dry run (the order as checked on the node), and a new
  price, tip or amount goes through the same checks as a new order. A sell whose all-in price goes down or whose amount
  goes up counts its whole custody under the token caps; a buy whose all-in price goes up counts its whole KAS under the
  KAS caps, as a new order would. Such a move with no reference to check it against is refused: cancel and place instead.
- `kob_claim` with `sell_price` is banded as a sell, and the claimed tokens count under the token caps.
- `kob_send` pays only your own addresses (`withdrawTo`).
- A `STOP` file in the data folder blocks every write (checked again right before the send).
- Every call is logged in `audit.jsonl`.
- Mainnet needs `"network": "mainnet"` and `"allowMainnet": true`.

For an agent without a chat, see [tools/mm-bot](../mm-bot/README.md). Tests: `npm test`; a live testnet-10 run:
[TN10-RUN.md](TN10-RUN.md).
