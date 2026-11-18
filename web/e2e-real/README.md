# Real-wallet run on Kaspa testnet-10

Drives the built KOB web app with the REAL released wallet extensions (KasWare, Kaspire, Kastle: versions pinned in
`tools/wallet-gate/lib/extension-pins.mjs`; the results tables below name the build each run used) against a testnet-10 node of your own (`KOB_TN10_WRPC`, default `ws://127.0.0.1:18210`; `NODE_WS` in
`.env` overrides it) and proves, per wallet: issue a token of the default program (the published public-mint build, 3 / 3 slots), place and cancel the main order types, amend an order,
merge the token's UTXOs and read My orders / Balances, and the Create token features (a creator share lock and its Unlock, a sell curve in the
genesis, Linear and Chromatic, a Collected buy filled from a curve and its Claim), with every accepted transaction checked on the node independently of the app.
Test money only. On demand only: CI never runs it (it needs extensions, funded keys and a node; `.github/workflows/ci.yml` says so).

`--mock` runs the SAME scenarios, selectors and checks offline against the mock stack of `web/e2e` (mock indexer + mock node that runs every
transaction through kob-wasm) with the in-page mock wallets of `src/testing/mock-wallets.ts`: it keeps the run in step with the UI and the protocol
on any machine. It proves nothing about the real extensions, their popups or a real node (see "What still needs a live run").

**Status.** The scenarios are ported to the current UI and protocol (base-unit amounts, the current ticket with its one order button and
inline status line, the My orders flows without a confirmation screen, the compact header wallet chip) and run offline with `--mock` ("Verified offline" below) and on TN10 with the real KasWare and Kaspire ("TN10 run of
2026-10-20" below: every scenario PASS; "TN10 run of 2026-11-14": KasWare 0.11.5 and Kaspire 0.5.31, issue, limit, amend, cells, merge PASS; "TN10 run of
2026-11-19": the same builds, lock, curve, claim, chromatic, unlock PASS with both, issue and limit again); Kastle 2.61.0 cannot connect to the app (its provider script throws under the app's Trusted Types).

## Run

```
cd web
npm run fetch-sdk && npm run build:wasm -- --copy    # once (kob-wasm bindings and the vendored SDK)

# TN10, real extensions (the owner's machine: .env, extension cache, a TN10 node)
node e2e-real/run.mjs                                 # KasWare, Kaspire and Kastle, every scenario (market SKIPs without an indexer)
node e2e-real/run.mjs --indexer http://127.0.0.1:8080 # also the market scenario (an indexer of the same TN10 node; or INDEXER_URL in .env)
node e2e-real/run.mjs --wallet kaspire --only limit,amend --headless
node e2e-real/run.mjs --ext latest --wallet kasware   # the NEWEST release, downloaded without a pin into .ext/latest/ (the pinned cache is untouched); version + sha256 land in the report

# offline, mock stack + mock wallets (any machine, no secrets; about 20 min per wallet, most of it the app's acceptance wait)
node e2e-real/run.mjs --mock                          # node-only, like the TN10 default
node e2e-real/run.mjs --mock --indexer                # with the mock indexer: market scenario and position cards
```

Flags: `--wallet kasware,kaspire,kastle|all` (default all), `--only <ids>` (default all, in this order: `lock,limit,ladder,amend,ioc,stop,oco,ifd,pair,market,fill,curve,claim,chromatic,cells,merge,balances,unlock`;
the issuance always runs first), `--indexer [url]` (TN10: that indexer, or `INDEXER_URL` of `.env`; `--mock`: the mock's), `--skip-issue` (TN10:
reuse the tokens of the previous run of that wallet from `out/token-<wallet>.json`, with its tracker entries and placement records), `--headless`
(Chromium `--headless=new`: works for Kaspire, KasWare needs a headed browser; `--mock` is always headless), `--keep-open`, `--rebuild` (rebuild
`dist-real/`), `--port N`, `--fund-below KAS` / `--fund KAS` (a wallet below the first is topped up with the second from the DEV key; defaults
120 / 100; one issuance with limit, amend, cells and merge needs about 15 KAS, so lower both when the DEV key runs short). `NODE_WS=ws://...` and `INDEXER_URL=...` in the environment override `.env`.
The app is built into `dist-real/` (`npx vite build --outDir dist-real --emptyOutDir`, done automatically when missing; `--rebuild` after a source
change); `dist/` is never touched.

Exit codes: `0` every selected scenario passed or was skipped with a reason, `1` a scenario failed, `77` SKIP with a message (no `.env`, no DEV key,
no extension cache, node unreachable, no SDK vendor / kob-wasm node bindings, failed app build).

Prerequisites of the TN10 run (all gitignored, never printed): `e2e-real/.env` with `NODE_WS`, `DEV_PRIVATE_KEY`, `DEV_ADDRESS`, optionally
`INDEXER_URL` and `WALLET_MNEMONIC_KASWARE` / `_KASPIRE` / `_KASTLE` (a missing mnemonic is generated into `.env` on first use: a throw-away
wallet the run funds), the extensions in `e2e-real/.ext/<wallet>` (copied from the wallet-gate cache, or downloaded; only the pinned version and
sha256 is accepted), Playwright's bundled Chromium (branded Chrome >= 137 ignores `--load-extension`). Each wallet gets a fresh profile in
`.browser-profiles/<wallet>` (kept after the run, so a stuck order can be recovered by re-opening it). A wallet below 120 KAS is topped up with
100 KAS from the DEV key (`--fund-below` / `--fund` change both; two issuances leave 10 KAS each in their token UTXOs; an IFD locks about 17 KAS while it is open). The market scenario
also spends from the DEV key: about 3 KAS for the counterparty bid, returned by its cancel.

## Scenarios (per wallet, in this order)

Every placement, cancel, amend and merge goes from the app's button straight to the wallet (the app's own pre-sign checks run silently; a
check that stops the order shows one line, `order-status` / `flow-status`, and stops the run), the wallet popup approved on TN10, then the node: the covenant outputs of a placement are unspent, the outpoints a cancel / amend spends are gone and
its own covenant outputs exist (the submitted transaction is read from the page's wRPC frames, or from the HTTP submit body with `--mock`, not
from app state). What a placement orders is checked on the app's final confirmation (`confirm-order`: title, net effect, the order's terms)
BEFORE it goes to the wallet; a confirmation that says something else is cancelled there, so the check leaves no order behind. After a placement
the app's placement record must point at one of those covenant outputs; the Portfolio's Open orders tab (`#/portfolio/open`)
must list the order as live (`order-row-<id>`, `data-live="1"`; polled: a fresh placement first shows `unknown` until the node's verdict on its
record arrives); after the cancel it must no longer be live there (its Order history row, `#/portfolio/history`, is recorded), and tokens a cancel releases must be tracked by the app's token tracker
(`kob.tokens.v1.testnet-10.<pubkey>`) as an output of the cancel. Rows, balances and status lines are read in one pass from the page (a cell a
row does not have is empty at once, never a 30 s locator wait). A failed scenario cancels the wallet's live orders before the next one starts.

| id | What it does |
|---|---|
| issue | Create token screen (`#/create`; the old `#/issue` leads there): the published public-mint build (`issue-program-public-mint` under Advanced, KCC20PublicMint, no minter; the default since 2026-11-04) is issued, 1,000,000 tokens with 8 decimals to the wallet. Result panel token id / txid, the registry entry (`template_id` `kcc20-ref-public-mint`, `max_token_inputs` 3), the genesis UTXO tracked with program KCC20PublicMint. Token B (the pair's quote) is issued the same way when `pair` is selected. The run then serves a test registry with both tokens listed. |
| limit | Limit GTC sell 3 @ 50 KAS (while it rests, Balances shows 3 in orders), cancelled, 3 tokens back; limit GTC buy 2 @ 0.5 KAS, cancelled. |
| ladder | One click on a limit buy ladder of 3 levels (1 token from 0.5 KAS, 2 % apart): one transaction and one wallet approval per level; a level funded from the change of the one before is asked for only once KOB's node lists that change and 2 s passed (`spendsPrevious` records which). Every level's covenant outputs unspent, a placement record each; then each level is cancelled. |
| amend | Limit sell 2 @ 50, My orders `order-replace-<id>` -> amend dialog (`amend-price` 55, `amend-preview`) -> one cancel-replace transaction; the SAME order id lives on at 55 (in-place AMEND record), then cancelled, 2 tokens back. |
| ioc | IOC buy 2 @ 0.4 with a 50 s life (advanced field `life`; an IOC may have 1 to 600 DAA = 60 s at most, counted from the ticket's clock, so the wallet's approval eats into it: 60 s would run over the limit), cancelled while alive. An IOC that a keeper on the network killed at its expiry before the cancel could be signed is a SKIP with its status. |
| stop | Stop-market sell 2, stop 0.2: rests unarmed, cancelled, 2 tokens back. |
| oco | OCO sell 2, take-profit 60, stop 0.2, cancelled, 2 tokens back. |
| ifd | IFD buy-first 2 @ 0.5 with exit take-profit 1, cancelled from its row (node only) or its position card (`position-cancel-<id>`, with an indexer). |
| pair | On the pair page `#/market/<A>/<B>`: limit sell 2 A @ 5 B (escrows A) and limit buy 1 A @ 0.5 B (escrows B), each cancelled, the escrow back. |
| market | Needs an indexer (SKIP otherwise): the DEV key (`maker` with `--mock`), driven through the same app in its own browser with an in-page signer (`features.test`, injected config only), rests a limit buy of 1 A @ 0.6; the wallet market-sells 1 A into it once the book shows it (the confirmation's auction must start from 0.6; a `clock-moved` stop, nothing sent, is tried again up to 3 times). If a matcher fills it, that is recorded; else it is cancelled. The counterparty bid is cancelled when still live, and whatever of the DEV key's orders is still live after a failed run is swept (cancelled). |
| fill | TN10 with an indexer only (SKIP otherwise, and with `--mock`): a taker fills the wallet's orders with `kob take --yes` and the DEV key, bounded by `--max-price 0.5KAS` / `--min-price 0.5KAS` (`KOB_BIN`, else `target/{release,debug}/kob` of this repo; the key goes in the environment, never on the command line or into `out/`). The wallet's limit sell 2 @ 0.5 is filled (the taker pays, the wallet's order ends), then its limit buy 2 @ 0.5 is filled by the taker delivering tokens; each wallet order must end not live in My orders. |
| cells | Three limit sells of 1 token (@ 50, 51, 52 KAS) are placed and cancelled one by one: every cancel releases a separate token UTXO, so the wallet holds three cells for the merge scenario that follows. |
| merge | My orders > Balances: `orders-merge-token-<A>` ("Merge all", its tooltip "Merge your n UTXOs of this token into one: about +x KAS back. ...") -> `merge-all-dialog`, whose `merge-all-summary` must say "Merge your n TICKER UTXOs into one: T transactions" with T = ceil((n - 1) / 2) -> `merge-all-confirm` -> one flow of T transactions, one wallet approval each; the last merge's single token output is unspent on the node and holds the whole supply, and the button is gone. |
| balances | Balances panel: for A and B free = 1,000,000, nothing in orders, no strays; no order left live. |
| lock | Runs first (its time runs during the rest). Create token, keep all, "Lock my share until" a date: the lock UI offers whole days or a date (the start of that day in the viewer's zone), so the app page gets a time zone (`GMT+hh:mm`, DevTools `Emulation.setTimezoneOverride`) whose next midnight is about 6 minutes away while the creation is planned and signed. Checks: the confirmation names the lock, the KCC-23 declaration has `lock` and its script pushes the lock time, the result panel shows the locked holder, Portfolio > Balances shows "locked until" and NO Unlock before the lock time. TN10 only, in a second page of the same wallet with the test hook (`features.test`) and its clock moved past the lock time: Unlock shows, the press asks the node's past median time and is refused (nothing sent); with that check taken out (`window.__kob.services.node.getPastMedianTime` replaced by the harness) the lock coin (1 KAS to the lock's P2SH) is signed and accepted, the unlock (lock time = the lock's time) is signed by the wallet and must be refused by the node; the locked genesis output stays unspent. |
| curve | Create token, "Sell on a curve", Linear (10% kept, 10 levels, 40 KAS if sold out, last level 5x): the curve is made unbound in the genesis; Open orders shows one Curve row "Level 1 / 10", Balances 900,000 in orders. The app refuses a buy into the wallet's own curve ("would trade against your own order"), so the counterparty (DEV key, in-page signer, as `market`) creates the same curve and the wallet places a crossing Collected limit buy of two levels at 1.15x the first level's price ("Plain limit" crossing, minimum fill one level): an executor on the network fills one level of the curve into it (`--mock`: the mock fills both). Then `claim`, then the buy, the counterparty's curve and the wallet's own curve are cancelled (the own one against its genesis, its 900,000 back as tracked tokens). Needs an indexer with an executor. |
| claim | Inside `curve`: Balances' Claim of the tokens the buy collected, the claim dialog (amount, network fee, carrier), one approval; the claim's covenant outputs unspent on the node, one level of tokens tracked as the wallet's from the claim, the buy still open with nothing bought. |
| chromatic | Create token, "Sell on a curve", Chromatic (10% kept, 1 octave: 13 levels, top 2x the first, 20 KAS if sold out): the form says "13, top 2x first", the last level is 2x the first, Open orders "Level 1 / 13"; cancelled against the genesis, 900,000 back. |
| unlock | Runs last: waits for the lock time + 95 s (the app shows Unlock 90 s after it), Unlock on Balances, one flow (the lock coin of the early check, or a new one first), the locked genesis output spent, the whole supply tracked as the wallet's from the unlock, "locked until" gone. A press the app refuses because the node's past median time is still before the lock time is tried again every 20 s (up to 10 minutes). `--mock` moves the page clock instead of waiting. |

Covenant-signing gate: a wallet the app does not know to sign covenant inputs (Kastle) is asked once for a test signature before its first order
(`order-covenant-check`); the run approves it. A wallet the app then refuses (`order-covenant-unsupported`) fails the order scenarios with the
app's text: that is a finding about the wallet, not about the run.

At the end of each wallet, any order still live is cancelled (best effort, recorded as `sweptOpenOrders`).

## Files

* `run.mjs` CLI, preflight, per-wallet flow, summary table. `scenarios.mjs` the order scenarios and node checks. `create.mjs` the Create token
  scenarios (lock, unlock, curve, claim, chromatic). `driver.mjs` node checker (official
  SDK), wallet drivers (real extensions or mock wallets), app driver (connect, the order button, My orders flows, the issuance confirmation screen).
* `mock.mjs` the `--mock` mode: mock stack in-process on a free port with an empty chain, the mock wallet page script (transpiled from
  `src/testing/mock-wallets.ts` with vite's oxc transform, so plain Node 22 runs it), the mock node checker.
* `server.mjs` static server of `dist-real/` (`http://localhost:<port>`, a secure context) with the generated test registry at
  `/registry-tn10.test.json` (the templates of `registry/tokens.example.json` marked reviewed + the issued tokens, listed; also saved as
  `out/registry-tn10.test.<wallet>.json`).
* `common.mjs`, `fund.mjs` helpers (extensions, onboarding, popups; DEV-key funding). `out/` (gitignored): `real-<wallet>.json` /
  `real-mock-<wallet>.json` (txids, popup texts and screenshots, status lines, node checks, records, balances), `real-summary.json`,
  `screens/<wallet>/*.png`, `token-<wallet>.json`.

## What changed against the 2026-09-29 run (port to the current UI and protocol)

The previous scenarios typed lots (`order-lots`, removed: amounts are whole tokens in `order-amount`), relied on the old ticket fields, issued
a token without choosing the program (the issuance now offers the public-mint build by default and the standalone 3 / 3 program under Advanced; the run asserts the
public-mint one and its registry entry), marked the test registry `verified` for a badge the UI no longer has, stopped at the old confirmation screen
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
`.env` exits 77 with its SKIP message. Timing with the mock (2026-10-20, after the one-pass row reads): about 7 minutes per wallet for everything.

## TN10 run of 2026-10-20 (real extensions, indexer of a live soak on the same TN10)

The indexer was a soak's executor (`--indexer http://127.0.0.1:8091`) while two soak executors filled, killed and refunded orders on the same
TN10 permissionlessly. Tokens reused with `--skip-issue` (issued by the first run of the day, issue PASS for both wallets).

| Wallet | limit | amend | ioc | stop | oco | ifd | pair | market | merge | balances |
|---|---|---|---|---|---|---|---|---|---|---|
| KasWare 0.10.0 | PASS | PASS | PASS | PASS | PASS | PASS | PASS | PASS (separate run, after the approver fix) | PASS | PASS |
| Kaspire 0.5.1 | PASS | PASS | PASS | PASS | PASS | PASS | PASS | PASS | PASS | PASS |
| Kastle 2.61.0 | no connect button: `window.kastle` is never defined on the app's page | | | | | | | | | |

Harness findings fixed on the way (none of them an app defect): every row / balance cell / status line was read through locators that wait
Playwright's 30 s default for an absent element (a limit has no stop cell), so a 90 s "no longer live" poll took ONE read, on the first render
after the cancel (indexer a second behind): "still live" after every cancel, and an IOC read late enough to be killed by a keeper; the ticket no
longer has a disclosure panel (`order-disclosure`), so "what is ordered" read an empty string (pair, market); KasWare 0.10's Sign window says
"Reject", not "Cancel", and the approver waited its whole 16 s on every signature (19 s per KasWare signature, longer than any auction lead:
market always stopped with `clock-moved`; 4.6 s after the fix); a submit request carries no transaction id (taken from the node's answer);
a scenario that failed after placing left its order live and failed `balances` too (now swept at once).

Kastle: its `injected.js` (2.61.0) sets `innerHTML` (goober, the CSS-in-JS of its toast library) while it loads, before it assigns
`window.kastle`; the app's CSP `require-trusted-types-for 'script'` (since 6ba73839, 2026-10-13) makes that assignment throw, so the provider
never exists on the app's pages (it does on a page without the CSP). Kastle 2.60.1 was proven before Trusted Types were enforced.

## TN10 run of 2026-11-14 (KasWare 0.11.5, Kaspire 0.5.31, indexer of a live soak on the same TN10)

The proof of the two-step creation of master 9d83d9db2 (wallet/chain.ts: the preparation is signed and SENT, the genesis is asked for only
once the wallet's own node can see the preparation's output 0, wallet/visibility.ts) with the real extensions. Fresh throw-away wallets funded
from the DEV key (`--fund-below 40 --fund 30` for KasWare, `--fund-below 30 --fund 40` for Kaspire: the DEV key held 90 KAS),
`--only limit,amend,cells,merge`, `--indexer http://127.0.0.1:8091` (INDEXER_URL). KasWare headed, Kaspire `--headless`.

| Wallet | issue | limit | amend | cells | merge |
|---|---|---|---|---|---|
| KasWare 0.11.5 | PASS | PASS | PASS | PASS | PASS |
| Kaspire 0.5.31 | PASS | PASS | PASS | PASS | PASS |

The creations (preparation, then genesis; `issue` records both txids, the KasWare `pushTx` calls the app made, and the time between the two
approval windows):

| Wallet | Preparation | Genesis (token) | Announce | Between the approvals |
|---|---|---|---|---|
| KasWare 0.11.5 | dd1a511e62912c104084d6991e0c1ed2fadbfbb335f3ccc0efb79a296e4db9bd | 78bcab07d7b65f10fed82e7c60bdae285e0a499710904e260ca5147a21dd4b12 (22684f7b6e..., RAKWXEKK) | `pushTx` accepted in 249 ms | 1.03 s |
| Kaspire 0.5.31 | 1305af5b6678e8688bc79e378a25312937521dbb3e63c6dca4ec63c4e290b4a1 | 7faa3b5c4417eb9fd0bd7a60242db5c595a24b8dd7db7f98e52ec56091dd5e75 (e595a4ffc7..., RAKPKXXQ) | none (no `announce`): KOB's node lists the output, plus 1.5 s | 2.61 s |

KasWare 0.11.5 creations of the day: 9 genesis windows, 8 with Sign. `pushTx` answered within 255 ms every time it was recorded, either accepted or "already
in the mempool" (the wallet's node had it from the network first; the app counts that as visible); the next window opened 1.02 to 1.03 s after
the first closed. ONE genesis window (preparation 92064f0b74915916cb5d0a8e0a977b2c7a60eb84cb836846dfdd09317196eecc, an early run whose
`pushTx` outcome was not yet recorded) showed only Reject: "Critical Signing Warning: Input #0 (92064f0b...:0) is not an unspent output on
this network", while KOB's node listed that output as unspent. 0.11.5 checks the input against its node's UTXO set, then its mempool
(`verifyPsktInputs` in its service worker); a transaction that just left the mempool for a block the virtual chain has not accepted yet is
in neither. Not reproduced in the 7 genesis windows after it; fixed the same day (*The visibility gate after the Reject-only window*, below).

Decline and resume (a separate script on the same driver, KasWare): the genesis window was rejected, the Create token page offered "Creation
of RRKWZXGE not finished" with "Finish the creation"; after a page reload it was still offered, the button asked for one approval (the window
opened 2.3 s after the click: the preparation, sent by the earlier page, waited for KOB's node plus the margin), and the token was created:
preparation c7291b6b58ed2afaad9919ad1857e6cf3a93001e2b4819d1a1e9f5fdfec61ff1, genesis
59cba93540ed67638ec459ba3f2c72c56d9aa73a31753789036dc4b23ac7e44d (token a8c6e7988306c412246889c482b6ed93e06b0a9bb7a1ba4b4c12e8d6a17c652c),
its output unspent on the node, the saved record gone.

kascov.io (`/data/testnet-10/tokens.json`, newest activity first) listed none of the day's tokens while its newest entry stayed about
240,000 DAA (6.7 h) behind its own tip for the whole session; the KCC-23 tokens of earlier runs are there by name and ticker
(`metadata_source` `kcc23_declaration`).

Harness changes (none of them an app defect):

* KasWare 0.11.5: the wallet screen no longer shows the address (only "Account 1"), so the switch to Testnet 10 is checked on Settings
  ("KASPA Network Testnet 10"); the address is still checked against the mnemonic after connecting. Its Sign window checks the inputs on its
  own node and withholds Sign when one fails: the approver waits for Sign or the "Critical Signing Warning", and on the warning rejects at
  once and records it (the run used to wait for the app's signing timeout, 4 minutes, with no reason).
* The header has one Connect: `wallet-connect` opens the chooser whose rows are `wallet-connect-<id>` (an extension plus the private key on
  testnet-10).
* The Portfolio shows one tab at a time (`#/portfolio/balances|open|history`, no "All" tab): rows are looked for on Open orders, the ended
  ones recorded from Order history, the Balances rows on Balances; the Balances cells are `role="cell"` divs (`dt-c-<key>`), not `<td>`s; an
  order's Amend is in its folded details (`order-toggle-<id>` first).
* The placement-record check takes the records of the placed transaction only: the app also adopts records of the wallet's other orders as
  the indexer lists them (orders an earlier failed run left live).
* `issue` records `sentTxids`, `pushTx` (KasWare's announce, read by wrapping `window.kasware.pushTx` in the page) and `gapsMs` (from the
  approval windows' `seenAt` / `doneAt`); a failed signing says what the windows showed.
* `--fund-below` / `--fund`.

## The visibility gate after the Reject-only window (TN10, 2026-11-14, KasWare 0.11.5, Kaspire 0.5.31)

KasWare's Reject in that window answers like any user rejection (`userRejectedRequest`, code 4001), so the app cannot tell the two
apart. Two gates were tried on the same day:

* Asking the wallet's own node (`getUtxoEntries`, polled until it listed the preparation's output 0): 10 of 10 genesis windows had
  Sign, but the second window opened 11 to 15 s after the first, because KasWare answers for its own account from its UTXO tracker,
  which lists an output only once it is mature. Dropped for the next one.
* The gate now: every fresh input waits until KOB's node lists it in its UTXO set, then 2 s, whether `pushTx` was accepted or not
  (wallet/visibility.ts). It is probabilistic: the race is the moment the wallet's node moves the transaction from its mempool into
  its UTXO index. A miss is safe: nothing is sent for that step, the app says so and why (`rejected-fresh`), and the step is asked
  for again (a creation's "Finish the creation", right on the page; a ladder or Merge all is run again).

Each row is one creation with KasWare, headed, 1 KAS per token output, driven by a loop over this driver; "between the windows" is
from the first window closing to the second one opening.

| # | Preparation | Genesis | `pushTx` of the preparation | Between the windows | Genesis window |
|---|---|---|---|---|---|
| 1 | 2a294f5cdf70ca98dc11c6a860e710d84e168b806ae572ee595e0cdfe77bd962 | 4ede46b1ccaa12740dd391f5d65c729a98420daddf70f7c145a3eb98de4a8c0b | accepted, 238 ms | 3.9 s | Sign |
| 2 | a3a06879f5610606dc5f0e058df07e60870ea980ac2e98710f64228b64931189 | 9291ff0b66970d3976c866613f4891974f55779e82d569731d58c93b1b5acf56 | accepted, 234 ms | 4.1 s | Sign |
| 3 | 330720583daf6fafe8afe30661b6fb55ef2162ed72cf2c5fbac0fd93a76fae88 | 6428b8a4652fadbb5ee06298b4a8413ecfa17d7025a934bbaf957ba6fcc580c7 | accepted, 257 ms | 3.8 s | Sign |
| 4 | a14ccccd6ba9d311ed7329acae2e782c3f79b3733f9a8abdf6ca2b031ea1b878 | 7227466a2d9e06d5ed58cacbd1e1a7efce4cd8c55cc6d286975a8279f6d2f1ed | already accepted by the consensus, 234 ms | 3.1 s | Sign |
| 5 | b63b110c6e80afb83345db4295415a915c4d5075db8def07cc1a627c89f1204e | ae3dd4cedf647e871a62de40d67955d09eacdb491a8233f076e6d0d596b36e44 | already in the mempool, 237 ms | 3.9 s | Sign |
| 6 | 5df0e079af11865b3559b08094941a61925765c731c570a25a37e07194d046f5 | 7ed5df8fbc5e89c6aea3fb6014b9d018e9045686da9aca5be7a373b6c3c2c884 | accepted, 241 ms | 3.7 s | Sign |
| 7 | 3a5e95d7265be5f0d5e58066d71cb71f2cf3132c8140cf875f5ad4169d4bdd85 | 89ade03576818de0ae87fb730483e4b136381f32ca20c6791fb930d110bed835 | accepted, 235 ms | 3.9 s | Sign |
| 8 | 89c24f276b58d88f5fb709f1f7399b0da97bee533bb448010c7b8cf636f806db | e700cfabef9bff8578a73f795ffa3658ddda00d220360cb5d994117f564d429d | accepted, 234 ms | 3.9 s | Sign |
| 9 | e584b9949c19a2a4850a277e27aa75bbefa686b25accbd859ee1e5bbbda088cd | 75b23d32641fe3caee36578552d1dde488da5e5deaba33906c703dc0aaedd32d | accepted, 235 ms | 4.7 s | Sign |
| 10 | b25366b9ae4ecbe0053303ac466b4d8498aa9dbeea001c99bf7c0d2b5cb2a4d2 | 565bf3e3c4fa4e4a0f509b48005e3e24901696bf4218ced8c5291465de5bf69e | accepted, 233 ms | 4.2 s | Sign |
| 11 | 78fecc0415aacf7b610e12d7dc7a4cc6acd9eb7c0092b8350736eb6852452689 | ad43489e06876057227ea0b39cdd4998156338c104f4bac2c620c97527c285ef | accepted, 234 ms | 3.9 s | Sign |
| 12 | 60250675e4dc764c75727578bcb558e4a539c912bc93968d15b38b87c95b63db | 08f6068f513fb639b9c6a9c5567754ccbedf561cca8d99817204582f9da9926b | already in the mempool, 234 ms | 3.9 s | Sign |

12 of 12 genesis windows had Sign, none Reject only, so "Finish the creation" was not needed live (the mock e2e `chain-sign` covers it:
a wallet node that lags the move, the first ask refused, finishing a moment later goes through). On the same build,
`node e2e-real/run.mjs --wallet kasware --skip-issue --only ladder,cells,merge --fund-below 0` (the ladder then was a buy ladder):
ladder, cells and merge PASS, no warning in any window; Merge all merged 4 UTXOs in 2 transactions (5fb599cbe1..., b5a5380e5f...),
the second spending the first's merged output. The buy ladder (672ac736d9..., 06dd260098..., 978772ebfa...): every window had Sign, 2.3 and 5.7 s
between them; the third level spent the change of the second, the second another coin of the wallet. A sell ladder from the one
merged token UTXO (`--only ladder`, a sell variant) chains on the token change: its second level (30b0cfa441...) spent the token
and KAS change of the first (5657f8de12...) 7.2 s after it and had Sign, but the ticket then planned the third without the second's
token change ("Not enough tokens: ... you hold 0"), so nothing was asked for it; the run swept the placed levels. That is a gap of the
ticket's token view between ladder levels, not of the visibility gate.

The private-key signer is not gated (it signs offline). Kaspire 0.5.31 (headless, 3 creations, 1 KAS per token output, on the earlier
gate with 1.5 s): its `signPskt` looks no input up on any node (background script), its `pushTx` and `getUtxoEntries` do not fit
the gate (wallet/kaspire.ts); windows 2.6, 2.8 and 2.6 s apart, every creation accepted (preparations 2211878ec960...,
7067c454b158..., 8f1cd0347e3d...).

## TN10 run of 2026-11-19 (KasWare 0.11.5, Kaspire 0.5.31, the Create token features of 06ff310ed)

The features added after 2026-11-14 (creator lock and Unlock, a curve sale in the genesis with the Linear and Chromatic shapes, the Claim of a
Collected buy, and the curve cancel of 57e0736fb), with the same throw-away wallets, the DEV key as the counterparty, and the indexer of a live
soak on the same TN10 (`INDEXER_URL`, an executor at commit 38e414d81, one before master). `--mock` first (both wallets, every scenario PASS),
then Kaspire `--headless` and KasWare headed; token outputs at 1 KAS (`issue-carrier`). Several runs: the table gives the passing one of each.

| Wallet | issue | lock | limit | curve | claim | chromatic | unlock |
|---|---|---|---|---|---|---|---|
| KasWare 0.11.5 | PASS | PASS | PASS | PASS | PASS | PASS | PASS |
| Kaspire 0.5.31 | PASS | PASS | PASS | PASS | PASS | PASS | PASS |

| Wallet | Step | Transaction |
|---|---|---|
| KasWare | issue A (genesis) | 0fcacd2d06ee9ec9769a7f5bc50fa4d5eb8b29513f93dc62857992d7e420978b |
| KasWare | lock: genesis, the whole supply locked until 23:14:00Z | 81530f9c8d33305fa742eab6dc0f8dad655fb1098accf14b01defcbde3e04b0d |
| KasWare | lock: the early lock coin (1 KAS to the lock), accepted | 90dc53095dff6c938d46883e6a670a292454b5560b5f3c5937fcce438949ab50 |
| KasWare | lock: the early unlock, signed by KasWare, refused by the node: "transaction input #0 is not finalized" | (not accepted) |
| KasWare | unlock, 151 s after the lock time (first press) | 98aa196cb85e31bef00df83cd3eccc3a2fd01eb0a29586ecb1a662189207b177 |
| KasWare | limit sell / its cancel | 4bc6ae9f8fc39791345ce832df153ceea03c3d1763f17f6f94a22e6e85522308 / 91bd4c746daa250c5a664db5d07302ce6aa835b0428728dbe9a02673e939d27f |
| KasWare | limit buy / its cancel | 66e383326bae8ee536312c0eef7b71d13bea155cfc06951bcee75f6506678221 / e4b7dec78d2f9746b34ef732d5faa884c62e147f915d08e79ca8844e29ee6e4b |
| KasWare | own Linear curve (genesis) / its cancel | 000ee9c5ecf674100ad50a36c25d47ec8bf006e869801803c6907baeb37e68af / fb0df22294da0d04f0632797f2538aa067f46889b990463cc264a09946174dae |
| KasWare | counterparty curve (DEV, genesis) | 123be050e50b3d206a2adf51ec3b59fb9565fd01cf5ba3eca806ede5052294d3 |
| KasWare | Collected buy (180,000 @ 0.0000201), one level filled by the executor in 36 s | daca46b033bcbdb6a5f5de7723a684b684f3e6c40f1b60849bcb2137db5af185 |
| KasWare | claim (90,000 to the wallet, fee 0.014948 KAS) | 7ffa787111b524361ff89a1bd0181534c179880035785efa97dc24cee92bec6c |
| KasWare | cancel of the buy / of the counterparty curve | 789b637ea6ef06f4705e590903e14ad4127e0ce67c4f27552ede4bc13b9f1f64 / 7b7a2b11c3d6837065525ad30d51a64a3638beb7cde01b7c4c019ff5dd236660 |
| KasWare | Chromatic curve (genesis, 13 levels) / its cancel | aa6ae9024a56e55ac7439610086b68ed439c4f0cce3a81bce322114bba74133a / fe927fe9af52ffde9b2d0ba177a0d99018bc939c32f7bee7125ede766d28bf24 |
| Kaspire | issue A (genesis) | 62bb65171b46cd1e3e4a6679e33dea37aa9034939b13c4f53a85c84a2343eaa9 |
| Kaspire | lock: genesis, locked until 23:02:00Z | e79884ce50df364f90d515ae423063f2e577d3290b31c8e3809a60b8a0de7ff6 |
| Kaspire | lock: the early lock coin, accepted | 1b2f7117a7f3ec43b5618294607334de931d95606fc61f0027e6d0d625889a6b |
| Kaspire | lock: the early unlock, signed by Kaspire, refused by the node: "transaction input #0 is not finalized" | (not accepted) |
| Kaspire | unlock, 150 s after the lock time (third press: the app refused the first two, the node's clock was still before the lock time) | 5ad4b92675011d3b8ccbde89bb778902a1de426617bfce8362df31c33cfdddd7 |
| Kaspire | limit sell, cancel, limit buy, cancel | 47a85cf2cd..., b5fd812c67..., 24cade4dca..., c44c55c3a1... |
| Kaspire | own Linear curve (genesis) / its cancel | cb2f103a523b7c6b83ae1c622e646fbcade0291c822eb0c0eb6cbb088aa13854 / a7bd03b7818c55b19b1bcc0df92fb22de329b306054766042809656dc98e373e |
| Kaspire | counterparty curve (DEV, genesis) | 5d7be4bb885eb937aafa827a7e8ccf9137a71e6509233b0e3cefc70a4ed2da93 |
| Kaspire | Collected buy, one level filled by the executor in 47 s | f4d268e9a4d8389d6e5fd38bf4bbc7411065dae4e527e0eae61480895d479358 |
| Kaspire | claim (90,000 to the wallet, fee 0.015188 KAS) | 99184469f309c60c385f4e993c0a0af6aa25790a4143b7079aa166634a3619c0 |
| Kaspire | cancel of the buy / of the counterparty curve | 368ae363afca59d88bd9cd9f6228a192948a3ede25d38acc4bc811bf32bbbe83 / b87cd2d40dda27cff148e5b5c4542f535be7bc4ec39ed6a94aedf0637291d837 |
| Kaspire | Chromatic curve (genesis) / its cancel | 2001f60391accbefe087cc40242c20289f3ccb653c1919fbb87a616f51f00fa3 / aa2ca92e70... |

KasWare `limit` passed in its fourth run (`--skip-issue --only limit`). Before it: the first stopped on the ticket field lookup (harness,
below); in the second and third KasWare's Sign window stayed on its loading spinner (a placement at 23:22Z, a cancel at 23:28Z: the
app's wallet timeout), and a buy that reached the node (e7c919eb744bbf1c0a301327af9fb76f1f6e62e3ef33d864190f1cd7af115f40) never
showed its transaction id on the ticket within 4 minutes; every order those runs left was cancelled by the run's sweep (the indexer lists
all of token A's orders cancelled). The same KasWare had signed every step of the lock, curve, claim and unlock runs minutes before, and
the next run passed: a slow wallet node is the likely cause (0.11.5 looks the inputs up on its own node before it shows Sign), not proven.

Findings (app; nothing changed in web/src):

* **The Unlock button comes before the node takes the unlock.** The Portfolio shows Unlock 90 s after the lock time
  (`UNLOCK_MARGIN_MS`), but TN10's past median time ran 127 to 130 s behind the wall clock (getBlockDagInfo, sampled at 22:35Z): a press at
  +95 to +125 s is refused by the app's own node-clock check (Kaspire: two refusals, then the unlock at +150 s). Safe (nothing is sent),
  but the user is offered Unlock and then told no.
* **That refusal shows the wrong sentence.** The app's text is "Not open yet on the network: the node clock is N min before the lock time.
  Try again then." (`orders.unlock.notOpen`), but the Portfolio banner shows the generic builder sentence "its time terms do not fit the
  current time (expiry, start or lock time)", the real one only under Details: `describeActionError` passes every message through
  `i18n/build-error.ts`, whose `/expir|lock ?time|deadline/` rule matches "lock time".
* **An unbound curve read from its placement record names the zero token.** KasWare: right after the genesis, Open orders listed the
  wallet's curve "from your records" (before the indexer had it) as "0.000017 KAS per 0000…0000", amount "900,000 / 900,000 0000…0000";
  read from the indexer (the Kaspire run, the counterparty's curves) it names the ticker.
* **The market page shows a just-cancelled order for a moment.** A second after the limit sell's cancel was accepted (Portfolio no longer
  listed it live), the token's market page still showed it under Open orders (1) as "unknown", "record only", with a Cancel button, while
  the book and Token details said no orders (both wallets); 40 s later it was gone. Cosmetic.
* **A locked token outside the registry reads "(unsupported program)".** Portfolio > Balances, locked tokens of earlier runs (not in the
  run's test registry): "(unsupported program)" under the ticker, though they are the public-mint program; unlocked tokens outside the
  registry do not say so.
* **A Collected buy's amount left counts its carrier.** After one level (90,000) of a 180,000 Collected buy at 0.0000201 KAS was filled,
  Open orders showed "~139,751.2438" left (the escrow's 1 KAS delivery carrier counted as buying power) beside "90,000 in the order".

Harness changes: Create token's program text is read behind its "?" and the confirmation's verified sections under All details (since
fce3f29ea); token outputs carry 1 KAS (`issue-carrier`; the TN10 default is 10 KAS, which each token UTXO keeps); the ticket's fields are
looked for inside the ticket (the market page's Open orders panel has its own `order-amount` cells: the stale row above made the lookup
ambiguous); a creation whose genesis was not sent is finished with "Finish the creation"; the counterparty's in-page signer skips its
KasWare-0.11.5 input check on TN10 (it asks the mock's HTTP node, so every preparation was refused); a flow that ends "Done" with a step the
node refused is read from its card; Balances is polled without re-opening the tab every few seconds (each opening reloads it, which takes
longer than that on TN10 with many token UTXOs). KasWare's onboarding once stopped on CreateHDWalletScreen and once its browser closed
during it: run again, it passed. The `fill` scenario was not run: `kob take --yes` now also needs `--token`.

## What still needs a live run (TN10, the owner's machine)

* Kastle (blocked: see above), its covenant-signing check.
* A market order a matcher fills (on 2026-10-20 none filled the test token's market sell: it was cancelled; a fill is recorded, not a failure).
* Command: `cd web && node e2e-real/run.mjs --rebuild` (add `--indexer <url>` for the market scenario). Commit the result table here.

## Earlier results

The run of 2026-09-29 (old protocol: lots, KasWare and Kaspire, issue / limit / IFD / OCO, all PASS on TN10) and its findings (minified build vs
SDK class names: `keepNames`; Kaspire `ordered-args` wiring; an IFD without an indexer is a row, not a position card) are in the git history of
this file.

Known noise: the app requests `/config.json` (optional, 404 on the static server). The mock node answers `/node/fee-estimate` by default.
