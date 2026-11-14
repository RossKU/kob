# Real-wallet run on Kaspa testnet-10

Drives the built KOB web app with the REAL released wallet extensions (KasWare, Kaspire, Kastle: versions pinned in
`tools/wallet-gate/lib/extension-pins.mjs`; the results tables below name the build each run used) against a testnet-10 node of your own (`KOB_TN10_WRPC`, default `ws://127.0.0.1:18210`; `NODE_WS` in
`.env` overrides it) and proves, per wallet: issue a token of the default program (the published public-mint build, 3 / 3 slots), place and cancel the main order types, amend an order,
merge the token's UTXOs and read My orders / Balances, with every accepted transaction checked on the node independently of the app.
Test money only. On demand only: CI never runs it (it needs extensions, funded keys and a node; `.github/workflows/ci.yml` says so).

`--mock` runs the SAME scenarios, selectors and checks offline against the mock stack of `web/e2e` (mock indexer + mock node that runs every
transaction through kob-wasm) with the in-page mock wallets of `src/testing/mock-wallets.ts`: it keeps the run in step with the UI and the protocol
on any machine. It proves nothing about the real extensions, their popups or a real node (see "What still needs a live run").

**Status.** The scenarios are ported to the current UI and protocol (base-unit amounts, the current ticket with its one order button and
inline status line, the My orders flows without a confirmation screen, the compact header wallet chip) and run offline with `--mock` ("Verified offline" below) and on TN10 with the real KasWare and Kaspire ("TN10 run of
2026-10-20" below: every scenario PASS; "TN10 run of 2026-11-14": KasWare 0.11.5 and Kaspire 0.5.31, issue, limit, amend, cells, merge PASS); Kastle 2.61.0 cannot connect to the app (its provider script throws under the app's Trusted Types).

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

Flags: `--wallet kasware,kaspire,kastle|all` (default all), `--only <ids>` (default all: `limit,amend,ioc,stop,oco,ifd,pair,market,fill,cells,merge,balances`;
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
| amend | Limit sell 2 @ 50, My orders `order-replace-<id>` -> amend dialog (`amend-price` 55, `amend-preview`) -> one cancel-replace transaction; the SAME order id lives on at 55 (in-place AMEND record), then cancelled, 2 tokens back. |
| ioc | IOC buy 2 @ 0.4 with a 50 s life (advanced field `life`; an IOC may have 1 to 600 DAA = 60 s at most, counted from the ticket's clock, so the wallet's approval eats into it: 60 s would run over the limit), cancelled while alive. An IOC that a keeper on the network killed at its expiry before the cancel could be signed is a SKIP with its status. |
| stop | Stop-market sell 2, stop 0.2: rests unarmed, cancelled, 2 tokens back. |
| oco | OCO sell 2, take-profit 60, stop 0.2, cancelled, 2 tokens back. |
| ifd | IFD buy-first 2 @ 0.5 with exit take-profit 1, cancelled from its row (node only) or its position card (`position-cancel-<id>`, with an indexer). |
| pair | On the pair page `#/market/<A>/<B>`: limit sell 2 A @ 5 B (escrows A) and limit buy 1 A @ 0.5 B (escrows B), each cancelled, the escrow back. |
| market | Needs an indexer (SKIP otherwise): the DEV key (`maker` with `--mock`), driven through the same app in its own browser with an in-page signer (`features.test`, injected config only), rests a limit buy of 1 A @ 0.6; the wallet market-sells 1 A into it once the book shows it (the confirmation's auction must start from 0.6; a `clock-moved` stop, nothing sent, is tried again up to 3 times). If a matcher fills it, that is recorded; else it is cancelled. The counterparty bid is cancelled when still live, and whatever of the DEV key's orders is still live after a failed run is swept (cancelled). |
| fill | TN10 with an indexer only (SKIP otherwise, and with `--mock`): a taker fills the wallet's orders with `kob take --yes` and the DEV key, bounded by `--max-price 0.5KAS` / `--min-price 0.5KAS` (`KOB_BIN`, else `target/{release,debug}/kob` of this repo; the key goes in the environment, never on the command line or into `out/`). The wallet's limit sell 2 @ 0.5 is filled (the taker pays, the wallet's order ends), then its limit buy 2 @ 0.5 is filled by the taker delivering tokens; each wallet order must end not live in My orders. |
| cells | Three limit sells of 1 token (@ 50, 51, 52 KAS) are placed and cancelled one by one: every cancel releases a separate token UTXO, so the wallet holds three cells for the merge scenario that follows. |
| merge | My orders > Balances: `orders-merge-token-<A>` ("Merge all (n): about +x KAS") -> `merge-all-dialog`, whose `merge-all-summary` must say "Merge your n TICKER UTXOs into one: T transactions" with T = ceil((n - 1) / 2) -> `merge-all-confirm` -> one flow of T transactions, one wallet approval each; the last merge's single token output is unspent on the node and holds the whole supply, and the button is gone. |
| balances | Balances panel: for A and B free = 1,000,000, nothing in orders, no strays; no order left live. |

Covenant-signing gate: a wallet the app does not know to sign covenant inputs (Kastle) is asked once for a test signature before its first order
(`order-covenant-check`); the run approves it. A wallet the app then refuses (`order-covenant-unsupported`) fails the order scenarios with the
app's text: that is a finding about the wallet, not about the run.

At the end of each wallet, any order still live is cancelled (best effort, recorded as `sweptOpenOrders`).

## Files

* `run.mjs` CLI, preflight, per-wallet flow, summary table. `scenarios.mjs` the scenarios and node checks. `driver.mjs` node checker (official
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
in neither. Not reproduced in the 7 genesis windows after it; fixed the same day (*The wallet-node check*, below).

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

## The wallet-node check (TN10, 2026-11-14, KasWare 0.11.5, Kaspire 0.5.31)

The visibility gate no longer trusts an accepted `pushTx` or KOB's node: an adapter with `walletSees` is polled until the wallet's own
node lists every fresh input (wallet/visibility.ts). KasWare answers it with `getUtxoEntries`, which reads the node its sign check
reads. For the wallet's own account KasWare answers from its UTXO tracker, which lists a new output only once it is mature (about 10 s
on TN10), so the second window now opens 11 to 15 s after the first instead of about 1 s. Each row is one creation with KasWare, headed,
1 KAS per token output, driven by a loop over this driver (`signOnConfirmScreen`). The wait is from the first
`getUtxoEntries` poll to the one that listed the preparation's output 0.

| # | Preparation | Genesis | `pushTx` of the preparation | Wallet-node wait | Between the windows | Genesis window |
|---|---|---|---|---|---|---|
| 1 | 2ce10ef5924a9dd3e5fd553736b4b3cb6ced4ab5e59fb842b8f13d10c50a4be3 | e5306854a39402c74d9f041d6ed8c115240f8f818229bef5d1d579cb35e47fef | accepted, 264 ms | 13.8 s (28 polls) | 14.6 s | Sign |
| 2 | b1f94b36beb14df41872cebb00d8e324d2f2f1770bf8ba314dc5e553b32d63d6 | 3be9b2f0149e9bc82632845f185253b5ba38a72098ab5543cc74543164eda30e | already in the mempool, 262 ms | 15.0 s (25 polls) | 12.5 s | Sign |
| 3 | 8182c944f6034c8e5256e91a67949774da6f02dc382120904c15850938a6baa9 | d03940810081f4deebbf15f0116a910451e839fa5f74082b81b5ca40c3a8a6ea | already in the mempool, 261 ms | 15.6 s (26 polls) | 13.0 s | Sign |
| 4 | a88312dac9d3101a9527aa9e78b9073a499a233c2dab2f0d872634f001f25f68 | c9600ce7eaaaab13733f3637ac1134923e35549d1ad2f70aa8753de7b5ded29e | already in the mempool, 262 ms | 14.0 s (21 polls) | 11.4 s | Sign |
| 5 | f972451397f50da763bd020f0e402eb85d0af129f179405613b0f8ac2f79d7d3 | 0beba8aadddc770fb4c87e5db92b11d5814548cbceb6e9142f4bd520bc0d9546 | accepted, 260 ms | 14.0 s (23 polls) | 11.4 s | Sign |
| 6 | 3203d6e2f0fc993b2f6d06a04de8f9429e2a4634e5bc13ba2898bf5d45bccf53 | 54ec199fe4ec4d4d90ff6c3e0023907ed98f1e79568e9aa50ff3c925193d6283 | accepted, 262 ms | 14.5 s (24 polls) | 11.9 s | Sign |
| 7 | 70960ac352dc7451ad20b6e0a42c7ccf7165db02ffdb7156428b10617bbec2f5 | 3af06d7881b6015ffdab6d81b0baef93ba9de97463f085cf2973691170ba8e7b | already accepted by the consensus, 263 ms | 10.2 s (21 polls) | 10.9 s | Sign |
| 8 | 0ba71ebc0163b177230b44b35404af0719ef4220e543780eadd40312e01e1b6e | 518d394eb4958e565da1dd38f221e1801879a5c160cf4111b00613753e7c78f4 | accepted, 262 ms | 15.0 s (25 polls) | 12.4 s | Sign |
| 9 | ce1c1ea805b4e665ff9397811e617bb5e9eca820d0a6bb25ffe478fc2b2cc190 | 9ab52d2cbeac3138aafcaa67cdd3c1342b96e86ee661e3e89b82f8f2ebc41357 | accepted, 261 ms | 15.5 s (26 polls) | 13.0 s | Sign |
| 10 | 323c347119bd34e35a28b0e84aa38ea8d9932b88af7995dd2615028539aaa1c4 | 31573c2a12d6577c9ee36f75f219b0b8531e441535a38408d72c87d44f239cef | already in the mempool, 259 ms | 14.1 s (23 polls) | 11.6 s | Sign |

10 of 10 genesis windows had Sign; no "Critical Signing Warning". `node e2e-real/run.mjs --wallet kasware --skip-issue --only
limit,cells,merge --fund-below 0` on the same build: limit, cells and merge PASS, no warning in any window. Again on Merge all (master
40b3da2ad, `--skip-issue --only cells,merge --fund-below 0`): cells PASS; merge PASS, 4 UTXOs in 2 transactions (9fdb2e43cb...,
8f23eeff9c...) in 20.6 s, the dialog's summary "Merge your 4 RAKWXEKK UTXOs into one: 2 transactions, one after the other, each approved
in your wallet (2 approvals)".

Kaspire 0.5.31 (headless, 3 creations, 1 KAS per token output): its `signPskt` looks no input up on any node (background script), its
`pushTx` and `getUtxoEntries` do not fit the gate (wallet/kaspire.ts), so it keeps KOB's node plus 1.5 s: windows 2.6, 2.8 and 2.6 s apart,
every creation accepted (preparations 2211878ec960..., 7067c454b158..., 8f1cd0347e3d...).

## What still needs a live run (TN10, the owner's machine)

* Kastle (blocked: see above), its covenant-signing check.
* A market order a matcher fills (on 2026-10-20 none filled the test token's market sell: it was cancelled; a fill is recorded, not a failure).
* Command: `cd web && node e2e-real/run.mjs --rebuild` (add `--indexer <url>` for the market scenario). Commit the result table here.

## Earlier results

The run of 2026-09-29 (old protocol: lots, KasWare and Kaspire, issue / limit / IFD / OCO, all PASS on TN10) and its findings (minified build vs
SDK class names: `keepNames`; Kaspire `ordered-args` wiring; an IFD without an indexer is a row, not a position card) are in the git history of
this file.

Known noise: the app requests `/config.json` (optional, 404 on the static server). The mock node answers `/node/fee-estimate` by default.
