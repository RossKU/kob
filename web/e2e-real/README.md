# Real-wallet run on Kaspa testnet-10

Drives the built KOB web app with the REAL released wallet extensions (KasWare 0.10.0, Kaspire 0.5.1) against a testnet-10 node of your own
(`KOB_TN10_WRPC`, default `ws://127.0.0.1:18210`; `NODE_WS` in `.env` overrides it) and proves, per wallet: issue a token, then place and cancel a limit sell, a limit buy, a buy-first IFD and a sell OCO.
Test money only. Kastle is not covered (optional, off in the app).

## Run

```
cd web
npm run build:wasm -- --copy           # once (kob-wasm bindings), plus the vendored SDK (npm run fetch-sdk)
node e2e-real/run.mjs                  # both wallets, everything (about 10 minutes)
node e2e-real/run.mjs --wallet kaspire --only oco --headless
```

Flags: `--wallet kasware|kaspire|all`, `--only limit,ifd,oco`, `--skip-issue` (reuse the token of the previous run of that wallet from
`out/token-<wallet>.json`, including its tracker entries and placement records), `--headless` (Chromium `--headless=new`: works for Kaspire,
KasWare needs a headed browser), `--keep-open`, `--rebuild` (rebuild `dist-real/`), `--port N`. `NODE_WS=ws://...` overrides the node.
The app is built into `dist-real/` (`npx vite build --outDir dist-real --emptyOutDir`, done automatically when missing); `dist/` is never touched.

Exit codes: `0` all selected scenarios passed, `1` a scenario failed, `77` SKIP with a message (no `.env`, missing DEV key or mnemonic, no extension
cache, node unreachable, no SDK vendor, failed app build): CI and other machines skip cleanly.

Prerequisites (all gitignored, never printed): `e2e-real/.env` (`NODE_WS`, `DEV_PRIVATE_KEY`, `DEV_ADDRESS`, `WALLET_MNEMONIC_KASWARE`,
`WALLET_MNEMONIC_KASPIRE`), the extensions in `e2e-real/.ext/<wallet>` (copied from the wallet-gate cache, or downloaded by `common.mjs`; each wallet is pinned by manifest version and
the sha256 of its CRX / zip in `EXTENSIONS` of `tools/wallet-gate/lib/extension-pins.mjs`, shared with the wallet-gate drivers, and another version is refused until the pin is moved there),
Playwright's bundled Chromium (branded Chrome >= 137 ignores `--load-extension`). Each wallet gets a fresh profile in `.browser-profiles/<wallet>`
(kept after the run, so a stuck order can be recovered by re-opening it on the same port). Wallets below 80 KAS are topped up with 60 KAS from the
DEV key (an IFD locks 41 KAS, every issuance leaves a 10 KAS carrier in the token UTXO).

## Files

* `run.mjs` CLI, preflight, per-wallet flow, summary table. `scenarios.mjs` issue / limit / IFD / OCO steps and the node checks.
* `driver.mjs` independent node checker (official SDK), wallet drivers (onboarding, popup approvers incl. KasWare unlock), app driver, sign step.
* `server.mjs` static server (`http://localhost:<port>`, a secure context) with the generated TN10 test registry at `/registry-tn10.test.json`
  (the templates of `registry/tokens.example.json`, which the example fixture marks `reviewed`, + the freshly issued token, listed, verified, lot = 1 token, tick 1000 sompi; also saved as `out/registry-tn10.test.json`).
* `common.mjs`, `fund.mjs` helpers. `out/` (gitignored): `real-<wallet>.json` (full record: txids, popup texts and screenshots, confirmation screen text,
  node checks), `real-summary.json`, `screens/<wallet>/*.png`.

## What is verified

1. Onboarding through the wallet UI (mnemonic import, switch to Testnet 10), connect popup approved, header shows the address and `testnet-10`.
2. Issue (fixed supply 1,000,000, 8 decimals, all to the wallet): confirmation screen, real wallet popup, node acceptance, token UTXO tracked by the app.
3. Limit sell (3 lots at 50 KAS), limit buy (2 lots at 0.5 KAS), IFD (buy 2 lots at 0.5, exit take-profit 1), OCO (sell 2 lots, take-profit 60, stop 0.2):
   each through the app's pre-sign confirmation screen (blocking findings would stop the run), the wallet popup, acceptance; My orders (indexerUrl EMPTY:
   rows come from the placement records resolved on the node) lists the order; cancel through My orders, again confirmation screen + popup; the order
   leaves the active list; for sells the app tracks the tokens released by the cancel.
4. Independently of the app, through a second SDK connection: every P2SH (covenant) output of an accepted placement is unspent on the node, the
   placement record points at one of them, and after the cancel the spent covenant outpoints are gone and the cancel's own covenant outputs
   (returned tokens / carrier) are unspent. The submitted transaction is read from the wRPC frames of the page, not from app state.

## Results (run of 2026-09-29, both wallets, exit 0)

| Wallet | issue | limit (sell + buy) | IFD | OCO |
|---|---|---|---|---|
| KasWare 0.10.0 | PASS | PASS | PASS | PASS |
| Kaspire 0.5.1 | PASS | PASS | PASS | PASS |

Transaction ids (all accepted on TN10):

| Wallet | Step | txid |
|---|---|---|
| KasWare | issue | 80d60b2ee45ff67bf5e15306b624ed200575193fe36c9e9ba8d0c3576b6b8436 |
| KasWare | limit sell place / cancel | e619444216781f8c05e1d8831621a634bcbec720627b74e0820dd33726221ebd / f4407b41706f0b9f5a0ea23b900d189cef45696dcea6764f661e1261855398e3 |
| KasWare | limit buy place / cancel | 598a2d13143c6815fb47e842149e24edc24900712e40e21cd30708bdfa790d16 / e9bcd6ed6800ea657428060639c7ed2c2c4c48f92a8538ce9246cd2727185f74 |
| KasWare | IFD place / cancel | 3f61022279513d3d99cbe9ef6e9c189e32f44266a83da8a61ddd84d0dbd02912 / 950a60555bfaae9ba887727fc9796d6a2479b7ab5188456ebf4b42b3cfa3396c |
| KasWare | OCO place / cancel | eebd3225531e3f2aa3f255203aa70b70a5bf6fb0fbdcdbbd6f5db4cf9f4858f7 / 66499802791194040b44fd3769a126ae63b2c17885e59b833c3f084a311c1360 |
| Kaspire | issue | 5b903fe774e9d2bac2116bd7624550152dd08a3bbeea59dfe5a769420c6d6789 |
| Kaspire | limit sell place / cancel | 39937c8e621c418c9efa62777201310fda53e8b21fe0329606b1a49a270f9faa / d179d82ddff6d765ee5fb1ffd4b0f48437bf1f8bc3f2af2d72bf49a7e63df536 |
| Kaspire | limit buy place / cancel | c20d5475dde4ff0b71f9b6f074927468c2dd5ecb67b46a6fdbffa838df0386a0 / b43c040cb420c92aadc2963eec5878beb734053e35cdb9c1c6e6b5f899ab7c4f |
| Kaspire | IFD place / cancel | 52051f78ea9c1c8ec6eb33c5c0fb1c47135c2e2b701e18242307e740744c3edb / a91fbc195941f611f4c59393cd4913d7408c9d513bb7a238ccc3b96f6911cec1 |
| Kaspire | OCO place / cancel | 363dc27df8a8107cf6e452aa5886d1f9e3b018c91b250706e68269e7cd0c1784 / daab972ba0d01c11f8bdbac6cf5ad8a9f8806bf11b929ed51bd114cab626e6e3 |

Timings: Kaspire signs in 5-7 s per transaction. KasWare needs 20-24 s per transaction, almost all of it the run's popup reader (it waits for the
popup text to settle before clicking Sign), not the wallet or the node (acceptance is seen within about 1 s).

## Popup observations (blind signing, as expected)

* **KasWare** ("Sign Transaction"): origin, balance change, inputs / outputs with addresses and amounts, fee, the raw KOB1 payload hex, and the
  generic disclaimer about unknown protocols. Covenant outputs appear as "Spend" to P2SH addresses; no order type, price, lots or token amounts.
  A profile that was closed and re-opened shows a KasWare "Unlock" window first (the run unlocks it).
* **Kaspire** ("Approve reviewed PSKT?"): txid, fee, wallet balance change, per-output "SIGNATURE-BOUND", "1 of N inputs selected", the warning
  "Input 0 is a covenant or non-standard script; Kaspire cannot verify its dApp business rules" and, for custody-token inputs, "partial signature". The
  request (scripts, mode per input) is in its Raw JSON. No token semantics. Modes seen on TN10: `ordered-args` for the order cancel inputs (the wallet
  emits the script itself), `wrap-signature` for the custody / token-transfer inputs of sell placements and OCO; both accepted (the app takes the signature
  and lets kob-wasm `finalize` assemble and verify the script).
* The app's own confirmation screen is what tells the user the order type, price, lots, locked KAS and escrowed tokens; its text is stored per step in `out/real-<wallet>.json`.

## Problems found by this run and fixed

1. Minified production build broke wRPC submission of covenant transactions: `Error converting property "covenant": size error: Slice must have the
   length of Hash`. The official kaspa-wasm SDK identifies its JS classes by `name`; the minifier renamed them (the same tx submitted fine with the
   raw SDK, in node, and in an unminified build). Fix: `build.rolldownOptions.output.keepNames` in `vite.config.ts`, with a regression test
   (`test/build-sdk-names.test.ts`, in-memory production build). The mock e2e could not see this: it submits over HTTP.
2. The app never enabled Kaspire's `ordered-args` mode (the adapter supports it, the provider was created without `dispatchTag`): wired in
   `src/app/context.tsx` with `dispatchTagFrom(kob)`.
3. In node-only mode (no indexer) an IFD entry without exits shows as one order row, not as a position card: the run cancels it with the row's cancel button. With the indexer available the entry, its exits and the repeat state are one position card (mock e2e: `e2e/orders/positions.spec.ts`).

Known noise: the app requests `/config.json` (optional, 404 on the static server).

## Re-run after merging main (2026-09-29): node-floor fee, KRON family, token-utxos, wallet events

KasWare 0.10.0, `node e2e-real/run.mjs --wallet kasware --only limit,ifd` (exit 0): issue, limit sell + buy (place and cancel) and buy-first IFD (place and cancel) all PASS with the
default relay-floor fee. Kaspire was not re-run here: its wallet held 77.5 KAS (below the 80 KAS top-up threshold) and the DEV key held 48 KAS (a 60 KAS top-up needs 62).

| Step | txid |
|---|---|
| issue | f8c67400e4aa053979409a90299008449480280d7efa31fb4d24d4e63bc1a04a |
| limit sell place / cancel | ac4485bea6ba6ea0d2613915f4469619b14110e59005203f68f9f08a9eb3ebe3 / b83391607ed736acdc4cb7ef5ec8df362ce22127d47c63ec1d26e3198f5c4679 |
| limit buy place / cancel | 75c0be708c6fac6f6f07a655d9989caab77b68f65b0d4251b41182680096213c / f17a63f3eb8e1a8af99bb3d50cbb8f4971f56765af8cebda6185bf4df1650c0f |
| IFD place / cancel | cfc3f33a915055105461fb4104a242ee5e3dad9aca262e93be02e49e25510154 / 51de596798483077639625505d3e89ff79d7811645757d9dc5b110181684620c |
