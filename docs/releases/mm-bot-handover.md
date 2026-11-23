# mm-bot on mainnet: handover

For the operator who runs `tools/mm-bot` on the mainnet KAS book of a dollar stablecoin (kzUSDC). Read it with
`tools/mm-bot/README.md`. The bot is pre-release and unaudited, like KOB itself. Its key is a hot key that signs without
asking: keep on it only what the bot needs.

## 1. What you run

- **The bot:** `tools/mm-bot` from the tag `v0.2.0` of the repository (`git clone --branch v0.2.0`). Node 22, no npm
  dependencies. It is not in the release archives.
- **The `kob` CLI:** the mainnet release archive of v0.2.0, `kob-0.2.0-mainnet-<target>` (`.zip` on Windows, `.tar.gz` on
  Linux). Do not use a development build.
- **Check the archive** before you unpack it (`docs/ops/release.md` 6):
  1. Its sha256 must equal its line in the release's `SHA256SUMS`: `<sha256 of the archive>`.
  2. `gh attestation verify kob-0.2.0-mainnet-<target>.<ext> --repo RossKU/kob --signer-workflow RossKU/kob/.github/workflows/release.yml --source-ref refs/tags/v0.2.0`
     must pass.
  3. Inside the archive, `kob` must match the archive's own `SHA256SUMS`.
  4. `kob --version` must say `kob 0.2.0 (mainnet build, commit <commit>)`.
- Set `kobCli` to the unpacked binary's path. Pin it: after any CLI upgrade, run `once --dry-run` again (the CLI's JSON may
  change).

## 2. The token it quotes

- Config key: `token.covenantId`, with `token.decimals` (6 for kzUSDC). `token.label` is only a name for messages.
- The bot pins the token by covenant id only. A ticker proves nothing: three tokens run the same Kozmic `BridgeMint` build and
  all verify (kzUSDC instance 2, the older kzUSDC instance 1 that is closed to deposits, and the Sepolia rehearsal Sepia.k).
- **Where the id is written down:**
  - your own (the issuer's) deployment record;
  - `contracts/third-party/kozmic/UPSTREAM.md`, *Tokens (mainnet)*: kzUSDC instance 2, `cc9957fe...`;
  - `tools/mm-bot/config.example.json`, `token.covenantId`.
  - KOB's registry does not list it. KOB curates no tokens.
- **Check it yourself**, all three must name the same 64 hex: `<64 hex>`.
  - `kob tokens --all --network mainnet`: find the row by covenant id. `DEC` must be 6 and `PROGRAM` must start with `kcc20-ref-public-mint`.
  - `kob book <covenant id> --network mainnet`: the book the bot will quote on.
  - Compare with the bridge covenant id your `BridgeMint` names. The web app shows it in the token details.
- Each cycle the bot checks `token.decimals` against what the CLI reports. On a mismatch every cycle fails with an error
  (no new quotes, and the quotes already out are not pulled; a STOP cannot finish either). End the process, fix the config,
  and start again.

## 3. Funding

With the defaults (`capitalUsd` 1000, 3 levels of 15 % a side, side cap 50 %):

| Asset | Needed | Why |
|---|---|---|
| KAS | about 450 USD worth, plus 50 KAS | bids; `quotes.kasReserve` (50 KAS) is never bid: fees and each order's 1 KAS carrier |
| kzUSDC | about 450 | asks |

- Fund the token first if the book has no KOB order of it yet.
- A short side goes idle and the bot logs what to deposit, to which address. It warns below 12.5 % of capital a side.
- **Fee caps:**
  - `safety.maxFeesKasPerDay` 50 KAS per UTC day. At the cap less `safety.cancelReserveKas` (5) the bot pulls its quotes
    and idles until 00:00 UTC. Cancels still go.
  - The CLI refuses any transaction whose fee is above 4 times the node's estimate. The bot never passes `--allow-high-fee`.
- Measured fees (TN10): place 0.002 to 0.010 KAS, amend about 0.004, cancel 0.004 to 0.013. Renewals of 6 quotes in a flat
  market: about 1 KAS a day.

## 4. Stop

- `node bin/mm-bot.mjs stop --config config.json` writes `<dataDir>/STOP` (default `./run/STOP`). Ctrl-C or SIGTERM do the
  same.
- The running bot sees the file within about a second. A CLI call already running finishes first (up to `cli.timeoutSec`,
  180 s). Then it sends a cancel for every quote it knows, re-checks every 5 s, and exits once the indexer lists none.
- In the TN10 run of 2026-11-16, STOP's 6 cancels were all sent within the minute (18:07 UTC).
- While the file exists the bot does not start. Delete it to start again.
- **STOP works only while the bot runs.** If the process is dead, its quotes stay until their expiry (soft, below). Cancel by
  hand: `kob cancel <id>`, or `kob cancel --view <dataDir>/order-views.json --network mainnet --key-file <key>` (checked
  on the node, no indexer needed; an order amended or filled since that view is refused as stale).

## 5. Alerts

- Every alert is one JSON line in `<dataDir>/logs/alerts.jsonl`: `level` (`info`, `warn`, `idle`, `error`), `state`, `reason`,
  `message`, `asset`, `shortfall`, `address`, `network`.
- `alerts.webhookUrl` (off by default): the same JSON, with `ts`, as one POST. No auth header, 10 s timeout, no retry; a
  failure is only logged. `check` prints the URL, so do not put a secret in it you would not print.
- What sends one:
  - a mode change: quoting, idle for `reference` (fewer than 2 venues), `jump` (2 % in a minute), `peg` (USDC/USD 0.5 %
    off), `fee-cap`, the cool-down, stopping;
  - `cycle-failing`: 3 cycles failed in a row (node, indexer or CLI), and `recovered` after;
  - `blind-pull`: quotes pulled without the indexer, or that could not be pulled;
  - funds: a side short or low, the fee reserve low, a reminder every 60 min, and "funds received";
  - a retired token's orders all cancelled.
- `node bin/mm-bot.mjs status --config config.json --json` shows mode, fair value, quotes, inventory and fees.

## 6. It trusts the public indexer

- Default indexer: `https://kob-api.kaspa.ag` (index-main). It runs on one Windows PC behind a Cloudflare tunnel, with no
  uptime promise (`mainnet-launch.md` 9). The bot reads its live orders, their state and its fills from it.
- **If the indexer lies** (internal review 2026-11-23, not fixed):
  - It can hide a live quote. The bot then forgets it, does not pull it, and STOP exits while it is still on chain. It rests
    at a stale price until its expiry and a refund.
  - It can report quotes as expired or not amended, or show a tiny amount left. The bot then renews every cycle, burns the
    50 KAS fee cap, and idles.
- **If the indexer is down:**
  - Each cycle fails. When a pull is due (STOP, no reference, a jump, the peg), the bot cancels from its saved views through
    the node.
  - A steady drift is not a pull. The backoff grows to 5 min, and only the 1-minute move is checked, so a drift of 1.9 % a
    minute never trips the 2 % guard. Quotes stay at stale prices.
  - Watch for `cycle-failing`. If it lasts, stop the bot and cancel by hand (section 4).
- **The way to remove this risk:** run your own indexer (`kobd --indexer`, keyless; `docs/ops/executor-quickstart.md`) and
  set `indexer` to it.
- **Expiry is soft.** Every quote carries a 90-minute expiry (`quotes.lifeMin`), renewed in place every 45 min. After the
  expiry conforming matchers stop filling it, but the chain cannot stop a fill at its limit price until a refund spends it.
  The README's "lapse by themselves" overstates this.
- **It can cross itself.** Buy amends go before sell amends, and neither `kob replace` nor the matchers check for the same
  maker. A move above the 1 % spread between cycles, or one failed amend, can leave its bid above its own ask. The loss is
  the gap plus tips and fees. The README's "never crosses itself" is not true for amends.
- **Bridge.** A bridge pause or a closed deposit route does not move USDC/USD. The bot keeps quoting. Watch the bridge and
  stop the bot if the backing is in doubt.

## 7. Pre-flight

1. `kob` checked as in section 1, its path in `kobCli`.
2. A key used only by the bot, readable only by its user, in `keyFile` (or `KOB_KEY`). Any other order of the token on that
   key is cancelled as a stale quote.
3. `network` is `mainnet`; `token.covenantId` and `token.decimals` checked as in section 2.
4. `token.usdFallback`: the default 1.0 is used when Kraken's USDC/USD is stale, and the peg check then passes. Set it to
   `null` to idle instead.
5. `indexer`: your own, or accept section 6.
6. `alerts.webhookUrl` set and tested, or `alerts.jsonl` watched.
7. Funding as in section 3, token first if the book is empty.
8. `node bin/mm-bot.mjs check --config config.json`: `config ok`.
9. `node bin/mm-bot.mjs once --config config.json --dry-run`: one cycle, nothing signed. Read the plan.
10. Start small: lower `capitalUsd` for the first day.
11. `node bin/mm-bot.mjs run --config config.json`. One bot per `dataDir`. It restarts 60 s after a crash.
12. Try `stop` once and see every quote cancelled.
13. The logs do not rotate. Watch the disk.

## 8. Contact

- KOB side: `<name, channel>`.
- Security issues: `SECURITY.md`.
- Your side, for the founder: `<name, channel>`.
