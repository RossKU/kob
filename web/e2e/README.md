# Mock e2e stack

Offline end-to-end testing of the KOB web app: a fake **indexer + Kaspa node** in one process, fake **browser wallets**, Playwright fixtures.
Everything runs against the real protocol code: `kob-wasm` builds, finalizes and validates, and the mock node runs every submitted
transaction through the same script engine (`kob.validate`), so a tx the mock accepts is consensus-valid.

## Run

```
cd web
npm run build:wasm -- --copy      # once: kob-wasm bindings (web/wasm)
npm run build                     # the app under test (dist/)
npm run e2e                       # mock server + vite preview + specs   (KOB_E2E_TARGET=app when dist/ exists)
KOB_E2E_TARGET=infra npm run e2e  # only the infra specs against the placeholder page (e2e/fixtures-site)
npm run mock                      # just the mock server (node mock/server.mjs [--port 8790] [--fund] [--no-seed] [--history])
node mock/server.mjs --history    # dev mode with 48 h of seeded trades, so the charts / stats / trades panels show a real market
npm test                          # vitest, includes test/mock-server.test.ts, test/mock-wallets.test.ts, test/e2e-infra.test.ts
```

Chromium comes from Playwright's local cache; if it cannot be resolved set `KOB_CHROME_PATH` to a `chrome.exe` (or `cache` to scan
`%LOCALAPPDATA%\ms-playwright`). The shared mock server holds global state, so specs run with one worker; `KOB_MOCK_PER_WORKER=1` gives every
worker its own in-process server (and enables parallel runs). Ports: `KOB_MOCK_PORT` 8790, `KOB_APP_PORT` 4173, `KOB_INFRA_PORT` 4174.

## Mock server (`mock/server.mjs`, `startMockServer({port})`)

* Indexer REST, shapes of `crates/kob-executor/src/indexer/reads.rs`: `/v1/health`, `/v1/health/ready`, `/v1/tokens`, `/v1/books/{token}`,
  `/v1/orders` (keyset cursor), `/v1/orders/{id}` (`state`, `custody`, `strays`, `children`, `auction`, `repeat`, `refund_due_daa`),
  `/v1/orders/{id}/events`, `/v1/fills`, `/v1/strays`, `/v1/token-events`, and `/v1/token-utxos?owner=&token=&role=&spent=`
  (paged `{items, next_cursor}` with the decoded KCC-20 `state`; the real endpoint is documented in `docs/ops/executor.md` 5.1). Errors: `{error:{code,message}}`. CORS is open.
* Market data (`mock/market.mjs`): `/v1/trades/{token}?limit=&before=` (a trade = the fill events of one tx; aggressor `side`,
  resting-side VWAP `price`, ask-side `amount` / `quote`, newest first), `/v1/candles/{token}?interval=1m|5m|1h|1d&from=&to=&limit=` (empty buckets
  omitted; `from` / `to` bound the bucket start), `/v1/stats/{token}` (24 h window ending at the mock chain time, book top, `null` when absent),
  `/v1/depth/{token}?levels=` (listed book merged by price per `price_basis`, cumulative sums). Unknown token -> 404 `not_found`, bad interval -> 400.
* Token/token pairs (`mock/pairs.mjs`, pair orders `KobPair` / `KobCondPair` / `KobIfdPair`): the endpoints of docs/ops/executor.md 5.4:
  `/v1/pairs/{base}/{quote}/book?depth=` (`direct` levels from resting KobPair orders, `entry` levels from if-done entries at their limit, `route`
  levels from a greedy walk of the two KAS books; exact `price_num / price_den` QUOTE base units per BASE base unit), `/v1/pairs?token=` (with entry
  and conditional counts), `/v1/pairs/{base}/{quote}/candles` (derived from the two KAS series, never from pair fills) and `/fills` (pair fills:
  volume and counterparty, `price_source: none`). Opt-in pair seed: `--pair`, `startMockServer({pair: true})`, `POST /mock/seed {pair: true}` or
  `POST /mock/reset {pair: true}` (EXUSD, 6 decimals, a KAS book, three KobPair asks, a KobPair bid, a buy-first IFD entry and a sell stop).
  Pair fills, arms and trails (`/mock/fill`, `/mock/arm`, `/mock/trail`, `mock/pair-fill.mjs`) are REAL kob-wasm batches signed by the mock's filler
  key: counterparty `via` inventory / netting / route, trigger evidence `mode` 0 (two KAS-book fills) or 1 (a resting KobPair).
* WebSocket `/v1/ws`: `subscribe` / `unsubscribe` / `ping`, channels `health`, `reorg`, `fills[:token]`, `book:<token>`, `order:<id>`, frames
  `{channel,type,data}`, `resync`.
* Mock node: `GET /node/info`, `POST /node/utxos {addresses}` (cashaddr decoded by `mock/address.mjs`, UTXOs indexed by script public key),
  `POST /node/submit {transaction}` -> `{transactionId}` or HTTP 400 `{error: "<text>"}`. Submit checks inputs (known, unspent, embedded UTXO entry
  equals the node's), then runs the whole tx through the engine. Accepted txs register orders (KOB1 records via `recoverOrders`), custody /
  stray / owned token UTXOs (states read from the KCC-20 leader's signature script, as the real indexer does) and close orders on cancel / refund;
  a cancel with a verified SWEEP record (same script, same covenant id from the order input) is a continuation: the order stays live at the new outpoint (event `sweep`).
  The DAA advances 10/s plus `/mock/advance-daa`.
* `GET /registry/tokens.json`: the example registry (`registry/tokens.example.json`); the fixtures point `registryUrl` here.
* Control (`POST /mock/...`, JSON): `reset` (default seed; `{seed:false}` empty; `{history:{...}}` adds a history), `seed` (`{tokens, orders, book, fills,
  trades, history, utxos}`), `utxo` (give KAS / tokens),
  `fill` (a REAL transition: new order UTXO + script, shrunk custody, payouts; IFD entries book their exit child), `arm`, `stray` (`{covenant_id, amount, token?, program?, ticker?}`: `token` = a FOREIGN stray of another token, a new covenant id is registered as an unlisted token; stray views carry `program` and `foreign`), `advance-daa`,
  `fail-next-submit`, `latency`, `health`, `reorg`, `resync`, `ws-close`; `GET /mock/submitted` (decoded summaries + the tx), `/mock/state`, `/mock/balance?key=`.
* Default seed: fictional token EXKCC (KCC20Ref_8x8, the 8/8 prototype kept as a fixture for the multi-slot paths; the
  issuance spec issues KOB's standard `KCC20Ref`; 8 decimals: order scale 1e8 base units per token; amounts in base units, prices in sompi per whole token), 10 price levels per side (best level holds 2 orders,
  maker = key `maker`), 8 finished orders with fills. Seeded orders are real (cancellable by their maker key). Nothing is funded
  unless `--fund` / `give`.
* Market history (opt-in, never part of the default seed): `--history`, `startMockServer({history: true | {...}})`, `POST /mock/seed {history:
  {token?, hours: 48, trades: 600, seed: 1, mid: 2500000, tick: 10000}}` or `mock.seedHistory({...})` in the fixtures. Deterministic per `seed`: a
  random walk with trend / volatility regimes that ends at `mid` (joins the default book), both aggressor sides, ~30% two-sided transactions
  (older resting order + aggressor). Single trades at explicit times: `POST /mock/seed {trades: [{token?, ts | ago_ms, legs: [{side, price, amount?,
  age_daa?, scale?}]}]}` (all legs filled in one tx; the older leg rests).
* Deterministic test keys `alice`, `bob`, `carol`, `maker` (`mock/keys.mjs`, `TEST_KEYS` in `fixtures.ts`). Test money only.

## Mock wallets (`src/testing/mock-wallets.ts`, `e2e/wallet.ts`)

`installMockWallet(page, {wallet: 'kasware'|'kaspire'|'kastle', secretKey, ...options})` injects `window.kasware` / `kaspire` / `kastle` with the
response shapes proven on TN10 (KasWare: safe-JSON string with `push(sig65)`, `kaspa_testnet_10`; Kaspire: `{psktTransactionJson}`, `scripts` modes;
Kastle: 33-byte key, P2SH inputs left unsigned without `scripts`, issue #353). Signing runs in the page with `window.__kobKaspa` (the app exposes
the official SDK there when its injected config has `features.test`). Options: `approve` / `rejectMessage`, `delayMs`, `wrongSignature`,
`dropScripts`, `omitInputs`, `failConnect`, `injectDelayMs` (late injection), `network`, `allowNetworkSwitch`. The handle steers a live page
(`configure`, `emit`, `setNetwork`) and exposes the call log (`calls()`, `waitForCalls()`): every sign request with the tx JSON, input indexes,
`scripts` and what was signed. Install before `page.goto`.

## Fixtures (`e2e/fixtures.ts`)

`import { test, expect } from './fixtures'`. `mock` (typed control client, reset to the default seed before each test), `appPage` (page with
`window.__KOB_CONFIG__ = {network:'testnet-10', indexerUrl, nodeUrl, registryUrl, features:{test:true, kastle:true}}`, the wallet installed and `/` opened;
fails on uncaught page errors), `wallet` (the handle). Options via `test.use`: `walletId` (null = none), `walletKey`, `walletOptions`, `appConfig`,
`autoOpen`, `appPath`, `failOnPageError`. `e2e/testids.ts` is the authoritative `data-testid` contract (`orderRow(id)`, `field(name)` ...).
The specs in `e2e/infra/` prove the stack with the placeholder page; the per-order-type specs are in `e2e/orders/` (below).

`e2e-real/common.mjs` holds the helpers for the real-wallet TN10 runs (unpack / cache extensions, launch, onboard, approve popups); it runs nothing on import.

## Order-type matrix (`e2e/orders/`, helpers in `e2e/helpers/`)

Every spec runs the real UI against the mock stack with the mock KasWare wallet (Kaspire / Kastle where stated), the default seed (EXKCC around
0.025 KAS, registry tick 100 sompi per whole token) and a served registry in which EXKCC is listed and its template is marked `reviewed` (a test patch of the example registry). `e2e/helpers/`:
`env.ts` (registry patch, `openMarket`, `fund`, clock helpers), `ticket.ts` (`pickType`, `fillFields` by intent path, `readDisclosure`,
`readConfirm`, `openReview`, `acknowledgeAndSign`, `placedBy`, `expectWalletSigned`), `orders.ts` (`seedOrders`, `openOrders`, `runFlow`),
`i18n.ts` (leaked-key and horizontal-overflow checks).

| Spec | Covers |
|---|---|
| `orders/cases.ts` + `orders/matrix.spec.ts` (46 tests, one per type and side; all figures derived by hand from the typed inputs) | limit GTC (sell, buy, with tip), limit GTD (date), limit day (against `kob.dayOrder`, deadline 00:00 UTC), timed activation, IOC, FOK, market (expected vs worst = 3% and 1% bounds), streaming, close, TWAP, DCA, Dutch and rising bid, stop-market, stop-limit, trailing stop, take-profit, OCO, IFD buy-first / sell-first (and with a stop exit), IFO buy-first / sell-first, IFD with a stop entry, repeat IFD (default unlimited, explicit count 3), repeat IFO. Each: disclosure -> review -> decoded confirmation (no blocking finding, net effect, carriers, exit card) -> sign -> wallet call log -> mock node -> indexer state fields + `recoverOrders` -> My orders label and status |
| `orders/manage.spec.ts` | cancel (single, partly filled), cancel-replace, cancel-all of a position (entry + exit in one tx), of a token and of everything, refund of an expired own order (`advance-daa`), stray sweep, escrowed balances panel, export -> clear local data -> import, My orders with the indexer down (records + node) and cancel from there |
| `orders/positions.spec.ts` | position cards (indexer available): IFD in progress (phase chip, price path, amount entered / exited / open, realised fills on demand), repeat IFD (re-arms left, entry and exit sub-rows), an unfilled entry as a card and its cancellation (history, phase cancelled), Japanese |
| `orders/guards.spec.ts` | self-trade prevention (limit, market, both sides), FOK pre-check (depth, counterparty slots), tick rounding shown, amount / price / expiry (>90 d, past, activation) errors, funding shortfall |
| `orders/wallets.spec.ts` | Kaspire (placement + cancel, bare signatures), Kastle with `scripts`, wallet rejects (neutral), wrong signature (finalize error, nothing broadcast), node refusal (retry) and stale-transaction refusal (re-plan), network mismatch, blocking finding on the confirmation screen |
| `orders/wallet-events.spec.ts` | account switch, network switch and wallet lock AFTER connect with the confirmation screen open, for KasWare / Kaspire / Kastle: stale plan blocked, nothing reaches the wallet or node, session follows / recovers, listeners removed on disconnect |
| `orders/sweep.spec.ts` | the maker's sweep in place: an ask with 2 own strays + 1 foreign stray outside the registry (strays panel flags, "Sweep strays", the confirmation text, the order live at the new outpoint with the same state and amount, strays spent, the maker's balances up, then still cancellable), a bid swept with a wallet coin (escrow unchanged), and "Sweep first" when a cancel would abandon strays (sweep, then cancel from the follow-up banner) |
| `orders/lifecycle.spec.ts` | exit lifetime of IFD / IFO exits (GTC default commits the "never"; a date: disclosure row, decoded exit card, template, and the exit a mock fill creates ends on it), timed start (`activeFrom`) of stop / take-profit / IFD / repeat IFO entries (disclosure, confirmation, state, GTC still from placement, >90 d refused), repeat ladder (3 repeat IFD levels: ladder rows, one confirmation + signature + transaction per level, stepped entry / exit prices at the node; a ladder below zero is refused) |
| `orders/i18n-mobile.spec.ts` | the ticket / confirmation / My orders flow in Japanese (terms, no leaked dictionary keys on any page), 390 px viewport (no horizontal scroll, dialog fits, controls reachable) |
| `ui/*.spec.ts` | the UI smoke specs (ticket, market, orders, issuance) |
| `ui/pair.spec.ts` | token/token pair view (`POST /mock/seed {pair: true}`: EXKCC/EXUSD, EXUSD listed by the spec's registry patch): the book's three sources (direct, entry, via KAS), the unified pair ticket (the KAS ticket's order types, prices in EXUSD per EXKCC, tips in KAS): a limit sell through the decoded confirmation screen, My orders row and cancel, a book click prefilling a limit buy that escrows EXUSD, a pair stop with its trigger rule, an IFD buy filled into a position and cancelled as a position, more order types planned, "pair book unavailable" without the pair routes |

Run one file with `npx playwright test e2e/orders/matrix.spec.ts`, one case with `-g "IFD sell-first"`. The mock node also runs every covenant, so a
matrix test that passes proves the transaction the UI built is consensus-valid.
