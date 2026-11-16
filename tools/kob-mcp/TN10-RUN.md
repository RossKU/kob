# TN10 run, 2026-11-16 23:24 to 23:31 UTC

A live run on testnet-10 through the MCP protocol: an MCP client started `server/index.mjs` over stdio, as Claude Desktop
does, and called its tools. The wallet was a new throwaway key; the primary wallet (the only `withdrawTo` address) was a
second new key. The node was `ws://65.108.107.30:18210`, the indexer `https://kob-api-tn10.kaspa.ag`, the CLI a build of
commit 5d73884a1. Config: mode `confirm`, `perTxKas` 40, `perDayKas` 60, `maxFeeKas` 0.5, `priceBandPct` 25.

- Funding: 300 KAS to the agent key, in one transaction spending one confirmed UTXO:
  `ebcc6cba7650393971511801d0315560f14719be9e3de37df9fc7abca667f567`.

| UTC | Call | Result | Transactions |
|---|---|---|---|
| 23:24:56 | `kob_book TUSD`, `kob_book orders: true`, `kob_stats`, `kob_candles` | best bid 37.52720225, best ask 37.55723001, last 37.62519894 KAS | |
| 23:25:02 | `kob_fee_estimate` of a 0.35 TUSD bid at 35 KAS | 14.25223601 KAS out (12.25 escrow, about 2 KAS of carriers, fee 0.002236) | |
| 23:25:07 | `kob_place_limit` buy 0.35 TUSD at 35 KAS | plan `b2cbae123f99`, nothing signed | |
| 23:25:19 | the same with `confirm` | submitted; order `9e319c23265e8d2ab02292d06a3f94c087350a859ef091eacf89d7666f89b971` | `40ebb7243b17b5d3f7280ce032d6c22c37b0a4b4a5f4af524ec91d3347322df7` |
| 23:25:29 | `kob_cancel` of that order, plan, then `confirm` | submitted; the indexer shows it `cancelled` | `a092150acf025d4475c3f3d8f52ae5646d64de204561503758c7172a724ede4e` |
| 23:25:43 | `kob_take` without a bound | refused: `PRICE_BOUND_REQUIRED` | |
| 23:25:43 | `kob_send` 5 KAS to an address not on the list | refused: `NOT_ALLOW_LISTED` | |
| 23:25:46 | `kob_send` 5 KAS to the primary wallet, plan, then `confirm` | submitted | `ee7efc8d0b229176bdfbdec71669b589a34c774758fc185a02a30bee5038d9f4` |
| 23:25:57 | `kob_token_create` KMCPT, 2 decimals, 1,000 supply, plan, then `confirm` | submitted; token `479ee2a6baee2221465d473613dfda9fe01b72836857edb25f7017dd16f9036b`, 10.003922 KAS out (carrier and fees) | preparation `9624ed3ea42d7cb61e316cd35f35e5ec894bcf8d165e19f610bd1400a6c6bcc2`, genesis `94a64b917b1a1f951a9b9753f626964fe4d8b58c15f80a5071b7c48426a53f64` |
| 23:26:04 | `kob_place_limit` buy 1 TUSD at 35 KAS (38.00223602 KAS, under the 40 per transaction) | refused: `DAILY_CAP: 29.26200201 KAS spent today (UTC) + 38.00223602 KAS is above the 60 KAS per day` | |
| 23:26:30 | `kob_take` 0.3 TUSD of the best ask (order `1e7c7317da3d8b643cf54d2043ef62b651e205f07dc62560e747757e60423313`) with `max_price` 37 KAS | refused by the CLI before signing: "this fill pays 37.56126734 KAS per TUSD ... above your --max-price 37 KAS per TUSD: nothing signed" | |
| 23:30:42 | `kob_place_limit` with `data/STOP` present | refused: `KILL_SWITCH`; the CLI was not run; `kob_book` and `kob_status` still answered | |

The ledger ended the day at 29.26200201 KAS: the bid (14.25223601), the cancel's fee (0.003806), the send (5.002038)
and the token (10.003922). The agent key kept 284.987998 KAS free and the 1,000 KMCPT on a 10 KAS carrier; the primary
wallet holds the 5 KAS. No key or key path appears in the transcript, the audit log or the ledger.

Seen during the run:
- The first take of the best ask got "stale view: the node holds no unspent output of the order at its current
  outpoint": a soak bot had filled or amended it after the book was read. The next read gave a live order.
- This CLI build reports a take's bound refusal on stderr (exit 1, stdout empty), not as `TAKE_PRICE_ABOVE_MAX` in
  `issues`; the tool returns the CLI's message either way.
- The dry run's covenant id and transaction id differ from the sent ones: the CLI plans the sent transaction again. The
  check on confirm is the KAS it takes, not the ids.
- The packed extension (`mcpb pack`) was unpacked and started with only the form's `KOB_MCP_*` variables; it listed
  the 17 tools and answered `kob_status`.
