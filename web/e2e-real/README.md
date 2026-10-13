# Real-wallet run on Kaspa testnet-10

Drives the built KOB web app with the REAL released wallet extensions (KasWare 0.10.0, Kaspire 0.5.1, Kastle 2.61.0, pinned in
`tools/wallet-gate/lib/extension-pins.mjs`) against a testnet-10 node of your own (`KOB_TN10_WRPC`, default `ws://127.0.0.1:18210`; `NODE_WS` in
`.env` overrides it) and proves, per wallet: issue a token of KOB's standard 3 / 3 program, place and cancel the main order types, amend an order,
merge the token's UTXOs and read My orders / Balances, with every accepted transaction checked on the node independently of the app.
Test money only. On demand only: CI never runs it (it needs extensions, funded keys and a node; `.github/workflows/ci.yml` says so).

`--mock` runs the SAME scenarios, selectors and checks offline against the mock stack of `web/e2e` (mock indexer + mock node that runs every
transaction through kob-wasm) with the in-page mock wallets of `src/testing/mock-wallets.ts`: it keeps the run in step with the UI and the protocol
on any machine. It proves nothing about the real extensions, their popups or a real node (see "What still needs a live run").

**Status.** The scenarios are ported to the current UI and protocol (base-unit amounts, the current ticket, confirmation screen and
My orders flows, the compact header wallet chip) and run offline with `--mock` ("Verified offline" below); the last TN10 run with real
extensions is the one of 2026-09-29 ("Earlier results").

## Run

```
cd web
npm run fetch-sdk && npm run build:wasm -- --copy    # once (kob-wasm bindings and the vendored SDK)

# TN10, real extensions (the owner's machine: .env, extension cache, a TN10 node)
node e2e-real/run.mjs                                 # KasWare, Kaspire and Kastle, every scenario (market SKIPs without an indexer)
node e2e-real/run.mjs --indexer http://127.0.0.1:8080 # also the market scenario (an indexer of the same TN10 node; or INDEXER_URL in .env)
node e2e-real/run.mjs --wallet kaspire --only limit,amend --headless

# offline, mock stack + mock wallets (any machine, no secrets; about 20 min per wallet, most of it the app's acceptance wait)
node e2e-real/run.mjs --mock                          # node-only, like the TN10 default
node e2e-real/run.mjs --mock --indexer                # with the mock indexer: market scenario and position cards
```

Flags: `--wallet kasware,kaspire,kastle|all` (default all), `--only <ids>` (default all: `limit,amend,ioc,stop,oco,ifd,pair,market,merge,balances`;
the issuance always runs first), `--indexer [url]` (TN10: that indexer, or `INDEXER_URL` of `.env`; `--mock`: the mock's), `--skip-issue` (TN10:
reuse the tokens of the previous run of that wallet from `out/token-<wallet>.json`, with its tracker entries and placement records), `--headless`
(Chromium `--headless=new`: works for Kaspire, KasWare needs a headed browser; `--mock` is always headless), `--keep-open`, `--rebuild` (rebuild
`dist-real/`), `--port N`. `NODE_WS=ws://...` and `INDEXER_URL=...` in the environment override `.env`.
The app is built into `dist-real/` (`npx vite build --outDir dist-real --emptyOutDir`, done automatically when missing; `--rebuild` after a source
change); `dist/` is never touched.

Exit codes: `0` every selected scenario passed or was skipped with a reason, `1` a scenario failed, `77` SKIP with a message (no `.env`, no DEV key,
no extension cache, node unreachable, no SDK vendor / kob-wasm node bindings, failed app build).

Prerequisites of the TN10 run (all gitignored, never printed): `e2e-real/.env` with `NODE_WS`, `DEV_PRIVATE_KEY`, `DEV_ADDRESS`, optionally
`INDEXER_URL` and `WALLET_MNEMONIC_KASWARE` / `_KASPIRE` / `_KASTLE` (a missing mnemonic is generated into `.env` on first use: a throw-away
wallet the run funds), the extensions in `e2e-real/.ext/<wallet>` (copied from the wallet-gate cache, or downloaded; only the pinned version and
sha256 is accepted), Playwright's bundled Chromium (branded Chrome >= 137 ignores `--load-extension`). Each wallet gets a fresh profile in
`.browser-profiles/<wallet>` (kept after the run, so a stuck order can be recovered by re-opening it). A wallet below 120 KAS is topped up with
100 KAS from the DEV key (two issuances leave 10 KAS each in their token UTXOs; an IFD locks about 17 KAS while it is open). The market scenario
also spends from the DEV key: about 3 KAS for the counterparty bid, returned by its cancel.

## Scenarios (per wallet, in this order)

Every placement, cancel, amend and merge link goes through the app's pre-sign confirmation screen (blocking findings stop the run; plan and
fresh-warning acknowledgements are ticked and recorded; the decoded kind is asserted: `create`, `cancel`, `cancel-replace`, `send`), then the
wallet (popup approved on TN10), then the node: the covenant outputs of a placement are unspent, the outpoints a cancel / amend spends are gone and
its own covenant outputs exist (the submitted transaction is read from the page's wRPC frames, or from the HTTP submit body with `--mock`, not
from app state). After a placement the app's placement record must point at one of those covenant outputs; My orders must list the order as live
(`order-row-<id>`, `data-live="1"`); after the cancel it must no longer be live, and tokens a cancel releases must be tracked by the app's token
tracker (`kob.tokens.v1.testnet-10.<pubkey>`) as an output of the cancel.

| id | What it does |
|---|---|
| issue | Issuance screen (`#/issue`): the standard 3 / 3 program (`issue-program-standard`, KCC20Ref) is the default, 1,000,000 tokens with 8 decimals to the wallet. Result panel token id / txid, the registry entry (`template_id` `kcc20-ref-3x3`, `max_token_inputs` 3), the genesis UTXO tracked with program KCC20Ref. Token B (the pair's quote) is issued the same way when `pair` is selected. The run then serves a test registry with both tokens listed. |
| limit | Limit GTC sell 3 @ 50 KAS (while it rests, Balances shows 3 in orders), cancelled, 3 tokens back; limit GTC buy 2 @ 0.5 KAS, cancelled. |
| amend | Limit sell 2 @ 50, My orders `order-replace-<id>` -> amend dialog (`amend-price` 55, `amend-preview`) -> one cancel-replace transaction; the SAME order id lives on at 55 (in-place AMEND record), then cancelled, 2 tokens back. |
| ioc | IOC buy 2 @ 0.4 with a 60 s life (advanced field `life`), cancelled while alive. |
| stop | Stop-market sell 2, stop 0.2: rests unarmed, cancelled, 2 tokens back. |
| oco | OCO sell 2, take-profit 60, stop 0.2, cancelled, 2 tokens back. |
| ifd | IFD buy-first 2 @ 0.5 with exit take-profit 1, cancelled from its row (node only) or its position card (`position-cancel-<id>`, with an indexer). |
| pair | On the pair page `#/market/<A>/<B>`: limit sell 2 A @ 5 B (escrows A) and limit buy 1 A @ 0.5 B (escrows B), each cancelled, the escrow back. |
| market | Needs an indexer (SKIP otherwise): the DEV key (`maker` with `--mock`), driven through the same app in its own browser with an in-page signer (`features.test`, injected config only), rests a limit buy of 1 A @ 0.6; the wallet market-sells 1 A into it once the book shows it (`order-market-start-ack` ticked when shown; the disclosure must start from 0.6). If a matcher fills it, that is recorded; else it is cancelled. The counterparty bid is cancelled when still live. |
| merge | My orders > Balances: `orders-merge-token-<A>` ("Merge (n)") -> the chain of transfers to itself (at most 3 token inputs each), one confirmation screen per link (`confirm-shown-as` states the merge); the last link's single token output is unspent on the node and holds the whole supply; the button disappears. |
| balances | Balances panel: for A and B free = 1,000,000, nothing in orders, no strays; no order left live. |

Covenant-signing gate: a wallet the app does not know to sign covenant inputs (Kastle) is asked once for a test signature before its first order
(`order-covenant-check`); the run approves it. A wallet the app then refuses (`order-covenant-unsupported`) fails the order scenarios with the
app's text: that is a finding about the wallet, not about the run.

At the end of each wallet, any order still live is cancelled (best effort, recorded as `sweptOpenOrders`).

## Files

* `run.mjs` CLI, preflight, per-wallet flow, summary table. `scenarios.mjs` the scenarios and node checks. `driver.mjs` node checker (official
  SDK), wallet drivers (real extensions or mock wallets), app driver (connect, confirmation screen, My orders flows).
* `mock.mjs` the `--mock` mode: mock stack in-process on a free port with an empty chain, the mock wallet page script (transpiled from
  `src/testing/mock-wallets.ts` with vite's oxc transform, so plain Node 22 runs it), the mock node checker.
* `server.mjs` static server of `dist-real/` (`http://localhost:<port>`, a secure context) with the generated test registry at
  `/registry-tn10.test.json` (the templates of `registry/tokens.example.json` marked reviewed + the issued tokens, listed; also saved as
  `out/registry-tn10.test.<wallet>.json`).
* `common.mjs`, `fund.mjs` helpers (extensions, onboarding, popups; DEV-key funding). `out/` (gitignored): `real-<wallet>.json` /
  `real-mock-<wallet>.json` (txids, popup texts and screenshots, confirmation texts, node checks, records, balances), `real-summary.json`,
  `screens/<wallet>/*.png`, `token-<wallet>.json`.

## What changed against the 2026-09-29 run (port to the current UI and protocol)

The previous scenarios typed lots (`order-lots`, removed: amounts are whole tokens in `order-amount`), relied on the old ticket fields, issued
a token without choosing the program (the issuance now offers the standard 3 / 3 program and the public-mint build; the run asserts the
standard one and its registry entry), marked the test registry `verified` for a badge the UI no longer has, stopped at the old confirmation screen
(no plan / fresh-warning acknowledgements, no decoded kind), cancelled through a single dialog (cancels now run in a My orders transaction flow,
`tx-flow` / `flow-done`), had no amend, IOC, stop, pair, market, merge or Balances steps, read only KasWare / Kaspire, and the node checker read
outpoints only in the wRPC form. All of that is replaced; Kastle is a third wallet (the app's optional `features.kastle`, switched on by the run).

## Verified offline (2026-10-13, `--mock`, WSL Ubuntu 24.04, Node 22.23, Playwright Chromium 1243)

Built as CI builds (`scripts/build-wasm.sh --install-bindgen --web --slim`, `npm run fetch-sdk`, `npm run build:wasm -- --copy`), app in
`dist-real/`. Every step, selector and node check of the TN10 run ran end to end; each signed step is a transaction the mock node accepted after
running it through kob-wasm (consensus-valid).

| Run | KasWare | Kaspire | Kastle |
|---|---|---|---|
| `--mock` (node only) | issue, limit, amend, ioc, stop, oco, ifd, pair, merge, balances PASS; market SKIP (no indexer) | same | same (the covenant-signing check ran and passed before the first order) |
| `--mock` (node only), again on the integrated UI of 2026-10-13 (header wallet chip, net-effect confirmation screen, private-key wallet) | same | same | same |
| `--mock --indexer`, again on that integrated UI | all 11 PASS, market included | all 11 PASS, market included | all 11 PASS, market included |
| `--mock --indexer` | all 11 PASS, market included (counterparty bid, market sell, both cancelled) | issue, ifd, pair, market PASS | issue, ifd, pair, market PASS |

Other checks: `node --check` of every module, `test/e2e-infra.test.ts` (12 tests, the shared `common.mjs` helpers), the TN10 preflight without
`.env` exits 77 with its SKIP message. Timing with the mock: about 31 s per signed step, almost all of it the app's own acceptance wait
(`tx-status` = `confirmed`); about 20 minutes per wallet for everything.

## What still needs a live run (TN10, the owner's machine)

* Everything wallet-specific: onboarding and the popups of KasWare 0.10.0, Kaspire 0.5.1 and Kastle 2.61.0 for every transaction kind above
  (amend, merge links, pair orders, IOC / stop / OCO are new to the TN10 run; Kastle has never been run here), Kastle's covenant-signing check,
  Kaspire's signature modes on the new inputs (token transfers of a merge, pair custody).
* The real node: wRPC submission of every new kind, acceptance timing, the app's acceptance status.
* Market against a real indexer (`--indexer` / `INDEXER_URL`) and whatever matcher runs on TN10 (a fill is recorded, not a failure).
* Command: `cd web && node e2e-real/run.mjs --rebuild` (add `--indexer <url>` for the market scenario). Commit the result table here.

## Earlier results

The run of 2026-09-29 (old protocol: lots, KasWare and Kaspire, issue / limit / IFD / OCO, all PASS on TN10) and its findings (minified build vs
SDK class names: `keepNames`; Kaspire `ordered-args` wiring; an IFD without an indexer is a row, not a position card) are in the git history of
this file.

Known noise: the app requests `/config.json` (optional, 404 on the static server). The mock node answers `/node/fee-estimate` by default.
