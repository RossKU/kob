# KOB executor: operations guide

`kob-executor` is one binary with role subcommands. It follows a Kaspa node's selected chain,
indexes every KOB order into a reorg-safe database, serves a read API (REST +
WebSocket), and, with the operator's hot key, matches crossing orders (arming and triggering stops inside its batches) and keeps them (refund,
kill, close). The normative rules are `docs/spec/matcher.md`, `docs/spec/order-types.md` and
`docs/spec/kob1-payload.md` (protocol v2.6); this guide covers operation.

Both token families of protocol v2.6 are handled end to end: **KCC-20** (112-byte token state, owner scheme `0x04`
custody) and **KRON** (46-byte token state, `id_type` 2 custody, no extension commitment, 4 token inputs / 5
outputs of at most 1e9 units per transaction). The KRON order kinds (`KobAskKron`, ..., `KobIfdAskKron`) are
indexed, listed, matched and kept like their KCC-20 twins; a book is one family's token. A pair order (`KobPair`, `KobCondPair`,
`KobIfdPair`) names the family of each of its two tokens in its state.

| Subcommand | Runs | Key | Reads its book from |
|---|---|---|---|
| `run` | indexer + read API + matcher + keepers, one process | operator hot key | the indexer's own store (in process) |
| `index` | indexer + read API | none | the node |
| `match`, `keep` | matcher, keepers | operator hot key | a book snapshot file (`--book-file`) |
| `x402` | x402 facilitator (verify, settle) | none | no book; finality from the node's UTXO set |

`run --x402-config <file>` adds the x402 facilitator to the one process (A.6).

The guide has three parts: **A** runs everything in one process (`run`); **B** is the indexer
(node, resources, storage, API, runbook); **C** is the matcher and keepers (economics, keys,
flags, metrics). Section numbers in B and C are stable (B 7.4 is the gap runbook).

Status labels: **[F]** verified (code, test, or measured), **[I]** inference or estimate that must
be measured during the TN10 soak, **[U]** unknown.

---

## Part A. One process: `kob-executor run`

```
node (wRPC JSON)
   |  VSPC v2 (High)                          kob-executor run
   v
 follower -> extract -> ingest (SQLite, one atomic commit per batch) -> read API (REST + WS)
                            |
                            +--> store: orders, custody, strays, acceptance of watched txs
                                   |  one read transaction per tick
                                   v
                         matcher + keepers: plan, sign, engine-validate
                                   |  submitTransaction (ids are watched by the follower first)
                                   v
                                 node
```

```
KOB_OPERATOR_KEY_FILE=/etc/kob/operator.key kob-executor run \
  --network testnet-10 --rpc-url ws://127.0.0.1:18210 \
  --data-dir /var/lib/kob-index --tokens /etc/kob/tokens.json --listen 127.0.0.1:8090 \
  --pause-file /run/kob/pause --metrics-file /var/lib/node_exporter/kob.prom
```

### A.1 One store, one follower, one truth

* **The matcher's book is the indexer's store.** `indexer::book::StoreBook` implements the
  matcher's `OrderBookView` over the SQLite tables; each tick reads orders, custody and strays in
  **one read transaction**, so they are of one cursor. What is listed is what
  `GET /v1/orders` calls listed: orders whose placement record was verified, whose current state
  was proven against the output's script public key, and (ask side) with exactly one live custody
  UTXO. An order with several custody outputs is left out (fails closed); a missing or wrongly
  sized custody is reported as it is and the matcher's `custody_ok` rejects the order.
* **Strays** are the live token UTXOs owned by an order id that are not its custody. They ride on
  the listed order, are never liquidity, and no matcher or keeper transaction spends them.
* **Day-order deadlines** come from the placement record (`orders.deadline`); **auctions, repeat
  entries and exits** are computed by the matcher from the proven state (`amountLeft`, `armed`,
  `stopPrice`, `rptAmount`, `parent`, `rptUntil`, `rptPrice`), as the API computes its views.
* **Families.** `orders.family` is the KOB1 family byte (1 KCC-20, 2 KRON) and the order's kind names it
  (`KobAsk` / `KobAskKron`). A KRON custody is the one live token UTXO owned by the order id with `id_type` 2
  and `is_minter` 0, exactly `amountLeft` base units; KRON token outputs are read from the
  `next_states` columns every KRON token input carries (each state checked against its output's script
  public key). A `TokenState` carries either layout, so the book, the strays and the matcher's planner
  handle both. The token allowlist may name a token's `family` (`kcc20`, `kron`): an order whose token
  program is of the other family is unlisted with `token_family_mismatch`.
* **Trigger evidence** is not stored (protocol v2.6: no receipts). A stop arms, ratchets or fills at its trigger only
  in a transaction that also fills a plain resting `KobAsk` / `KobBid`; the matcher plans those next to each other
  (C, "Stops: triggered and armed in the batch"). The indexer records which fill it was on the event
  (`detail.evidence`: input index, the evidence order's covenant id, side 1 ask / 2 bid, its quote) and derives the armed / ratcheted continuation (a
  trailing ratchet's new stop from the quotes of the plain orders the transaction fills).
* **"Done" means accepted.** Before it submits a transaction the runner registers its id with the
  indexer (`Ingest::watch_tx`). After every commit the follower records which chain block accepted
  each watched transaction, and forgets it when a reorg removes that block. Each tick the runner asks
  the store for the acceptance of everything it tracks: *accepted* when a chain block accepts it,
  *final* 100 DAA later, moved back to *pending* (and resent, idempotently) when the block is
  reorged away. There is **no second `getVirtualChainFromBlockV2` follower** in the process, and the
  book and the acceptance cannot disagree about the chain: a transaction the store has not applied
  yet is still pending, so its outpoints stay reserved and are never planned twice.
* **A lagging book plans nothing.** When the store's cursor is more than `--max-book-lag-daa`
  (default: the lag tolerance below, 300 DAA, about 30 s) behind the node's virtual DAA score, the runner builds and submits
  nothing (`kob_matcher_book_stale 1`) and keeps tracking acceptance. A fresh database catching up
  from the pruning point, a stalled follower or a `gap` show up this way instead of as plans against
  spent orders. `0` disables the check.
* **Nothing is planned before the indexer has caught up, or is within the lag tolerance.** The runner plans (matcher,
  keepers, maintenance jobs) only while the store is **caught up** (`following`, the state `/v1/health` reports as
  `caught_up: true`: its last poll left the cursor within 100 DAA of the node, or found nothing new) **or within the
  lag tolerance**: `max_lag_secs` (indexer config, default **30**, `daa_per_second` DAA each: 300 DAA at 10 BPS) and the
  follower is `following` or `catching_up`. `/v1/health` keeps `caught_up` strict and adds
  `within_lag_tolerance` (what the gate uses; true whenever `caught_up` is), `/v1/metrics` and the metrics file
  `kob_indexer_within_lag_tolerance` and `kob_indexer_lag_tolerance_daa`. Why a tolerance: under a heavy load a
  batch takes 10 to 30 s, so the follower reports `catching_up` although its book is seconds old, and a strict gate
  would idle the executor for as long as the flood lasts (the TN10 soak at 400 to 1,400 tx/s paused its bots for 448 s
  at a lag of at most about 30 s). A plan over a view that is a few seconds old is harmless: the covenants enforce every
  order's terms, a spent input only loses the race (`MissingInput` / `DoubleSpend`, counted as a conflict, nothing
  built from it is final), and the book-lag bound above uses the same number. Beyond the tolerance, and in the states
  `starting` (a restart: the store is where the last run left it until the first batch lands, a 141-block batch took
  24 s on TN10, and the TN10 soak's restarts lost maintenance sells to `MissingInput` that way), `node_unavailable`,
  `gap` and `stopped`, nothing is planned: `kob_matcher_waiting_for_indexer` is 1, the log says `waiting for the
  indexer to catch up` once, and no funding is read (the low-funds alert judges only a funding actually read).
  `max_lag_secs = 0` is the strict gate (only `following`, book-lag bound 300 DAA). The x402 facilitator's own book
  reads are not gated by it.

### A.2 Flags

`run` takes the indexer options (`--config`, `--network`, `--rpc-url`, `--data-dir`, `--tokens`,
`--start`, `--listen`, `--no-api`, `--no-record-log`, `--reorg-window-hours`; B 4) and the
matcher and keeper options (C, "Running"):

| Flag | Default | Meaning |
|---|---|---|
| `--key-file` | `$KOB_OPERATOR_KEY_FILE` | operator key file (C, "Keys and funds") |
| `--no-match`, `--no-keep` | off | switch a role off (not both: that is `index`) |
| `--max-book-lag-daa` | `max_lag_secs` x 10 (300) | plan nothing while the book is this far behind the node; 0 = never wait |
| `--tick-ms`, `--fee-rate`, `--dry-run`, `--pause-file`, `--metrics-file`, `--low-funds-kas` | as C | |
| `--no-fee-estimate`, `--fee-max-rate`, `--fee-max-tx-kas`, `--fee-refresh-ms`, `--fee-max-age-ms` | off, 1000, 1, 10000, 60000 | the fee policy (C, "Fee policy"): each transaction pays its urgency's bucket of the node's fee estimate within these bounds, the x402 facilitator's intent executions and expiries included |
| `--min-profit`, `--max-tx-bytes`, `--no-chain`, `--no-arm` | as C | matcher (`--no-arm`: no updates; stops the batch fills next to their evidence still trigger) |
| `--max-funding-inputs`, `--funding-target-utxos`, `--consolidate-funding` | 8, 4, 2 | how a batch picks the operator's funding (C, "Keys and funds") |
| `--no-maintenance`, `--no-dust-sell`, `--maintenance-max-jobs` | off, off, 2 | the operator's own token UTXOs: merge and sell (C, "Maintenance") |
| `--inventory-policy <file>` | none (off) | the surplus-inventory policy (C, "Surplus inventory"): the pair surpluses the operator may accumulate as inventory; maintenance never sells a listed token (also on `match`) |
| `--no-refund`, `--sweep-own-strays`, `--return-foreign-strays`, `--keeper-min-profit` | as C | keepers (`--keeper-min-profit` is `keep --min-profit`) |

`index` (with `replay`, `export-orders`, `import-orders`, `rebase`), `match` and `keep` stay as
standalone modes. The database and the record log are the same files in every mode; never run two
writers on one data directory.

### A.3 Deployment

* **Single host, one operator:** `run`, one systemd unit, the node on loopback. The read API and the
  hot key share a process: keep `--listen` on loopback behind your reverse proxy, fund the key with
  a few days of fees only, and keep `--pause-file` at hand.
* **Public API and key apart:** `index` on the public host (no key); on a private host a second
  `run --no-api` with its own data directory (its own follower and store). `match --keep
  --book-file` also works with a snapshot exported by an indexer, but that mode has no acceptance
  feed: it follows acceptance itself with a second VSPC follower (C, "Node").
* The node: B 2 (`--utxoindex` for the operator's funding lookup and the recovery tools,
  `--retention-period-days=7`, loopback RPC, never `--unsaferpc`). The process holds two loopback
  wRPC connections to it: the follower's and the matcher's (submission, funding).

```
[Service]
ExecStart=/usr/local/bin/kob-executor run --network testnet-10 --rpc-url ws://127.0.0.1:18210 \
  --data-dir /var/lib/kob-index --tokens /etc/kob/tokens.json --listen 127.0.0.1:8090 \
  --pause-file /run/kob/pause --metrics-file /var/lib/node_exporter/kob.prom
LoadCredential=kob-operator-key:/etc/kob/operator.key
DynamicUser=yes
StateDirectory=kob-index
NoNewPrivileges=yes
ProtectSystem=strict
ReadWritePaths=/var/lib/node_exporter
Restart=always
```

Keep the data directory on a path the unit may write (`StateDirectory=kob-index` is
`/var/lib/kob-index`).

**Deployment build.** Every build embeds the reference templates (protocol v2.6: no receipt covenant, no `R_ID`, no
genesis step; conditional orders arm in every build). The deployment variant only records its network: it embeds
`contracts/deploy/testnet-10/deployment.json` (the template hashes of the reference artifacts, see its `DEPLOYMENT.md`)
and refuses any network other than `testnet-10`:

```
cargo build --release --locked -p kob-executor --features deploy-tn10
scripts/build-wasm.sh --tn10 [--web]     # web/bot bindings: crates/kob-wasm/pkg-node-tn10, pkg-tn10
```

The templates are the same in both builds, so clients (web app, bots, `kob-cli`) place orders the indexer lists with
either variant; the variant is a guard against running a testnet binary on another network.

**Mainnet release build** (`docs/ops/release.md`): `cargo build --release --locked -p kob-executor --features
deploy-mainnet` embeds `contracts/deploy/mainnet/deployment.json`, refuses any network other than `mainnet` and pins
the token registry of the release (`registry/tokens.json`, sha256 in the record). Without `--tokens` it lists the
tokens of that embedded registry (`registry.source` = `embedded:registry/tokens.json` in `GET /v1/tokens`); with
`--tokens` it uses the operator's file and logs a warning when its hash differs from the pinned one.

### A.4 Monitoring

Both sets apply: `/v1/health` and its alarms (B 6), and the textfile metrics (C, "Monitoring").
`run` adds `kob_matcher_book_stale` (1 while the book lags; alert if it stays 1 for minutes: the
follower is behind, in `gap`, or the node is down), `kob_matcher_waiting_for_indexer` (1 while the follower is
neither caught up nor within `max_lag_secs`: same alert) and `kob_matcher_book_lag_daa`.

### A.5 Verification

`cargo test -p kob-executor --test x402_indexed`: the facilitator's finality from the indexer (A.6):
a settlement is accepted by the chain block the follower saw although the merchant spent the output at
once, reconcile does not call that a reorg, and a real reorg of the accepting block marks the entry
`ambiguous`; the matcher never drops the facilitator's watches; swap quotes from a book.

`cargo test -p kob-executor --test executor_e2e`: synthetic chain data built with the `kob-protocol`
builders goes through the follower into the store, and the matcher and keepers plan against it, every
transaction re-validated in the v2.1.0 engine. The listed book equals the chain's orders for every
kind; strays are reported and never spent; the matcher arms a stop with an update next to the plain fill
that is its evidence, and fills another at its trigger in the batch of its evidence (the indexer
records the arm, the fill and `detail.evidence`); a keeper refunds an expired order; the matcher
fills a crossing; acceptance, finality and a reorg rollback (back to pending, resent,
re-accepted, the book reverted) are read from the indexer while the test node refuses
`getVirtualChainFromBlockV2`.

### A.6 The x402 facilitator in `run`

`--x402-config <file>` (or `KOB_X402_CONFIG`; strict JSON, `crates/kob-executor/x402.example.json`)
serves the facilitator (`GET /supported`, `POST /verify`, `POST /settle`, `GET /health`, `GET /metrics`, and with
`invoices` the `/invoices` routes of A.7)
in the same process, on the configuration's `listen` address (bound before anything else starts). It
holds no key. Its `node` is the indexer's `--rpc-url` (the file's value is ignored) and its `network`
(`kaspa:testnet-10`, `kaspa:mainnet`) must be the one the indexer follows, or `run` refuses to start.

* **Fees.** The intent executions (high) and expiries (normal) the facilitator builds are priced with the runner's rates (C,
  "Fee policy"), within what the intent itself can pay; the payers' own transactions are submitted as signed.
* **UTXO facts and submission** go to the node (`getUtxosByAddresses`, `submitTransaction`), as in the
  standalone `kob-executor x402`.
* **Finality comes from the indexer's follower.** The facilitator registers every settlement's
  transaction with the indexer (`Ingest::watch_tx_for(Watcher::Facilitator, ..)`) before it
  broadcasts it; `accepted` is "a chain block of the current selected chain accepted it" and
  `confirmed` additionally needs the configured DAA depth. This holds when the merchant spends the
  payment output immediately (the UTXO observation alone would then never see the settlement) and
  turns false when the accepting block is reorged away: the periodic reconcile marks such an entry
  `ambiguous` (evidence stays consumed) only when the tracker, the UTXO set and the mempool all lost
  the transaction. The UTXO observation stays the fallback for transactions the follower did not see
  from before their broadcast (a restart) and while the indexer catches up.
* **Watches are per consumer.** The matcher's acceptance bookkeeping drops only its own watches.
* **Swap-and-pay quotes** are the payer's job (`docs/spec/x402-swap-and-pay.md` §12; the facilitator
  exposes no quote endpoint). `kob_executor::x402::quote` builds a `Quote` from any indexer book
  (`GET /v1/orders`, a snapshot file, or the store in process): plain KCC-20 GTC asks and bids with a
  constant price, best price first, skipping the orders an `order_conflict` named.

The standalone `kob-executor x402 --config <file>` (flags `--network`, `--node`, `--listen`,
`--ledger`, `--auth` override the file) runs the facilitator alone, with finality from the node's UTXO
set.

**Ledger format.** The ledger (`ledger`, a JSONL log) is read only in the format of the running build. An intent
payment recorded for an earlier router template (for example before the token-intent lock pin of 2026-10-06), an intent
record of any format this build does not decode, or an entry of an intent kind without its intent record stops the
facilitator at startup: `ledger line <n> holds an intent payment in a format this build does not read (...); stop, archive
this ledger (and its invoice store) and start with a new ledger path`. Such a line is never discarded as a torn tail and
never replayed as anything else; the file is left untouched. Archive the ledger and its invoice store together (an invoice's
payments live in the ledger), settle what they still hold by hand, and start on new paths. Only an entry recorded as a
direct payment is ever finalized from its own transaction's output; an intent payment is finalized only by an execution
of its intent.

### A.7 Intent payments and invoices

Two optional parts of the facilitator (`docs/spec/x402-swap-and-pay.md` sections 17 and 18), both off by
default and switched on in the x402 configuration file:

```json
"intents":  { "enabled": true, "keeperPubkey": "<64 hex x-only key>", "fillerSompi": 20000000,
              "maxAttempts": 20, "maxBuilds": 24, "maxCandidates": 8, "lockMarginDaa": 10 },
"invoices": { "enabled": true, "store": "x402-invoices.jsonl", "maxLifetimeSeconds": 604800,
              "publicUrl": "https://pay.example.com", "maxOpenPerMerchant": 10000,
              "maxExtraPaymentsPerInvoice": 16 }
```

**Intent payments** (`extra.route.binding = kob-intent-v1`). The payer signs one transaction, the creation of
a KOB router intent (`contracts/argent/kob_router.ag`, `docs/argent.md` "The router") that locks its funds
with its worst case as terms. The facilitator verifies it, dry-runs an execution against the indexer's book
(none builds: `intent_not_executable`, retryable, nothing broadcast), consumes the creation's inputs and the
intent outpoint in its ledger, broadcasts the creation and then executes the intent itself, as a keeper: it
plans against the book of the moment, records the execution in the ledger before it submits it, and when an
order it named is taken by another fill it remembers the order as lost and plans again, without the payer.
The periodic reconcile keeps driving intents whose `/settle` answered `settlement_pending`; no execution is
attempted after the authorization's expiry (an invoice's expiry caps it), and after `maxAttempts` the payment
fails.

**Expiry.** An intent that ended without an execution (its deadline passed, its attempts ran out, or no book could
execute it) still holds the payer's funds and stays executable by anyone until it is spent. From its deadline on the
reconcile expires it on chain (the router's `expire`, no signature: the intent's KAS back to the payer less at most
0.1 KAS, its locked tokens whole to the payer's key), at the fee policy's normal rate; an expiry that still waits in
the mempool a minute later is replaced (`submitTransactionReplacement`) at the high rate, within the router's 0.1 KAS
cap. A refused expiry is retried every reconcile until the intent's window ends (an hour after it was given up), then
every 10 minutes while the intent stands. The ledger records it in `intent.expiry` (`txid`, `rate`, `lastTryMs`) and
how it ended in `outcome`: `expired` (accepted), `spent` (the payer's cancel or a late execution: told apart by the
spending transaction), `never_created` or `unbuildable: <why>`. The payer does not depend on it: its cancel works
at any time, and the SDK builds the same expiry for anyone to submit (`expireIntent`) when the facilitator is gone.

* It needs the book: it is served only inside `kob-executor run` (`--x402-config`); the standalone
  `kob-executor x402` refuses `intents.enabled`.
* It holds no key. Executions need no signature (every input is a covenant). `keeperPubkey` only receives
  what token intents leave to the keeper (the spread above the merchant's amount and the intent's unused KAS)
  and their filler outputs (an output that holds an index open; `fillerSompi` each). A `KasToToken` intent
  returns its unused KAS to the payer.
* The router reads every token under the program the intent names (open ICC): pay assets on any KCC-20 program of
  the 112-byte state (3/3, 8/8, ...) or a KRON program, merchant tokens on a KCC-20 one (a KRON token is never
  received). KaspaCom's program (pending review) uses the payer-signed route (`kob-swap-v1`). A KRON intent is
  executed against the book's `KobBidKron`s.
* The executions' orders are reserved for the matcher of the same process while they are in flight, like
  payer-signed swap-and-pay routes.
* `/supported` lists `kob-intent-v1` in `routeBindings` and the `router` artifact id.
* Metrics: `kob_x402_intent_executions`, `kob_x402_intent_conflicts`, `kob_x402_intent_failed`.
* Ledger: an intent entry is keyed by the creation id; `intent.executions` lists every execution with its
  state (`live`, `dead`, `accepted`) and `intent.lost` the orders lost to other fills. The settlement response
  names the execution in `transaction` and the creation in `extensions.kob.intent`.

**Invoices.** Routes (on the same listener):

| Route | Access | |
|---|---|---|
| `POST /invoices` | merchant API key | registers an invoice (idempotent); answers `{ id, url, invoice, created }` |
| `GET /invoices/{id}` | public | the invoice; its canonical JSON hashes to `id` (what a QR code links to) |
| `GET /invoices/{id}/status` | public | `unpaid`, `pending`, `paid` (with the transaction and its DAA), `expired`, `failed`; attempts; refused duplicate and late payments |
| `POST /invoices/{id}/pay` | public, per-IP / per-site / anonymous rate limits, invoice-pay slots | settles one payment of the invoice (body: the x402 `PaymentPayload`); answers a `SettlementResponse` |

An invoice is registered only when every `accepts` entry is one the facilitator settles and within the
merchant's `allowedPayTo` / `allowedAssets`, its lifetime is at most `maxLifetimeSeconds`, and the merchant has
fewer than `maxOpenPerMerchant` unexpired invoices. A payment's request hash is the invoice id. One invoice is
paid once: while one payment of an invoice is being settled, any other request to pay it is answered
`invoice_pending` (retryable) at once instead of waiting for it; pays run in a pool of their own
(`maxConcurrentInvoicePays`, 16; `busy` / 503 beyond it), so anonymous pays never take the merchants' `/settle` slots. A
duplicate or late payment is refused before broadcast, kept as evidence, and reported in the
status (`extraPayments[].observed = accepted`) if its payer broadcasts it anyway, so it can be refunded by hand
(refunds are not automated). The pay route is public, so this evidence is bounded: a refused payment that spends exactly the outputs
a kept one spends (a fee variant of the same funding: at most one of them can reach the chain) is not kept, and an
invoice keeps at most `maxExtraPaymentsPerInvoice` (16) refused payments; further ones are refused without being written
(`kob_x402_invoice_evidence_dropped`). A direct payment whose outcome stayed unknown (`ambiguous`: the node was unreachable at
its broadcast, or its accepted output vanished in a reorg) and that the node still does not know an hour later (not in
the mempool, its merchant output not on chain) is failed, its outpoints are released and the invoice can be paid again
until it expires; the invoice keeps watching it as `extraPayments[].kind = released`, so a late acceptance is still
reported. The store (`store`, default the ledger path plus `.invoices.jsonl`; `:memory:` is
refused on mainnet) is an append-only JSONL log with an fsync per record and a lock file, like the ledger;
back it up with the ledger. Metrics: `kob_x402_invoices_registered`, `kob_x402_invoice_refused`; the reconcile
report counts `extraPaymentsSeen`.

A KAS invoice can also be paid with any wallet through `kaspa:<address>?amount=<KAS>` (the SDK's `kaspaUri`);
the facilitator does not see such a payment (no invoice status).

---

## Part B. The indexer

### 1. What it does

```
node (wRPC JSON)  ->  follower  ->  extract  ->  ingest (one atomic SQLite commit / batch)  ->  read API
   getVirtualChainFromBlockV2        KOB records only     record log appended + fsynced first      REST + WS
   verbosity High                    (nothing raw kept)
```

* One `getVirtualChainFromBlockV2` call per poll from the stored cursor, verbosity `High` (the
  lowest level that carries payloads, previous outpoints and signature scripts) [F]. The node
  returns **accepted** transactions only, in mergeset order; `removed_chain_block_hashes` is
  tip-first; `added` is capped (2,480 chain blocks at 10 BPS, and shorter when the batch is large)
  so the cursor always follows the returned `added` list [F].
* `removed` blocks are reverted tip-first, `added` blocks are applied in the order returned, and
  the cursor, the derived rows, the block-window pruning and the record-log position are committed
  in one transaction. The cursor never advances on an error [F, tested with an in-memory node and
  a property test that compares the incrementally maintained database with a from-scratch build
  and with a rebuild from the record log, under random traffic and random reorgs].
* **Discovery.** An order exists only if its KOB1 **placement record** (`matcher.md` 1.1) passes
  `kob_protocol::payload::recover_orders`: the pinned template hash, the canonical state, the P2SH
  of `prefix || state || suffix` equal to the output, a fresh covenant whose id is the consensus
  genesis id, and (ask side) a custody output holding exactly `amountLeft` base units owned by that id.
  Each record is validated alone, so one bad record in a payload hides only itself. The payload is
  never trusted; a missing or wrong record leaves the order invisible (a `rejects` row for wrong
  ones). A payload of version 2 or 3 describes orders of RETIRED templates (the protocol v2.6 layouts,
  `docs/spec/template-retirement.md`): its records never become orders (cancel only, never in a book) and each
  is counted as a reject `retired_template:<kind>` (e.g. `retired_template:KobAsk`, `retired_template:KobCrossKron`).
* **Lineage.** Every later spend is classified from the input's own signature script (the revealed
  redeem script must hash to the spent output and match a pinned template; the dispatch tag selects
  the entry, the arguments give the amount in base units). The continuation's state is *proven*, not assumed: the
  spent script's mutable windows (`kob_protocol::state::mutable_windows`: `amountLeft`, `armed`,
  `stopPrice`, `rptAmount`) are spliced with the values the entry could have written and the P2SH
  must equal the output's script. If no candidate matches, the state is stored as unknown (and the
  next spend reveals it).

#### Order kinds and what is indexed

| Kind | Indexed |
|---|---|
| `KobAsk`, `KobBid` | limit, GTD, timed, IOC / FOK, market and Dutch / rising auctions, TWAP / DCA; fills, cancel, refund |
| `KobCondAsk`, `KobCondBid` | stop, stop-limit, trailing, take-profit, OCO: **arm** and **trail** events (next to the plain resting fill that is their evidence, `detail.evidence`; the new stop is proven from the quotes of the plain orders the transaction fills), triggered fills (a stop leg filled at its trigger next to its evidence, `detail.evidence`), stop-band fills, cancel, refund |
| `KobIfdBid`, `KobIfdAsk` | IFD / IFO entries incl. **stop entries** (arm, triggered fills next to their evidence, band fills); every entry fill creates its **exit** from the entry's committed `exitState` with no new placement record; entry and exits are linked (`parent`, `children`) |
| repeat IFD / IFO | `rptAmount`, booked exits (`parent`, `rptPrice`, `rptPre`, `rptUntil`), the **merge** of a take-profit into its entry (`rearm` event on the entry, `merged_into` on the exit's fill), sell-first custody created or grown by a merge, an empty repeating entry (`amountLeft = 0`, no custody), `close()` of an empty entry, cancel-all of a position |
| `KobPair`, `KobCondPair`, `KobIfdPair` | pair orders (token A for token B, either family each, below): placement with one or two custodies, fills (route, netting, inventory) with the pair fields, the GTC rest, the IOC return, sell-out, refund / kill, cancel, arms and trails in both evidence modes, if-done exits, bookings and repeat merges, strays of either token |
| KRON kinds (`KobAskKron`, `KobBidKron`, `KobCondAskKron`, `KobCondBidKron`, `KobIfdBidKron`, `KobIfdAskKron`) | everything above, in the KRON family: placement records with the 2-byte custody part, `id_type` 2 custody, booked exits and repeat merges, triggers read from the KRON plain orders |
| KCC-20 and KRON tokens | custody and **strays** (below), the token identities seen |

* **Exact custody (C7).** For every token-holding kind the expected custody is
  `amountLeft` base units. Token outputs owned (scheme `0x04`; KRON `id_type` 2) by an order id are read from the
  KCC-20 transfer leader's `next_states` argument (KRON: the columns of a KRON token input; each state is checked
  against its output's script public key). The **custody** is the ONE token output owned by the order id
  that is exactly what the covenant rules require of the order's state after the transaction: its own token, `amountLeft`
  base units, plain (borrowing disabled, covenant-owned by the id), the order's extension commitment, created by the transaction that
  created or spent the order (the first such output; an exit's custody is the delivery of the fill that created it). Any other token
  UTXO owned by an order id is a **stray** (`matcher.md` 1.2). The ask covenants only guard token INPUTS, never outputs, so a taker can
  add extra outputs owned by an order id in an ordinary fill; the classification by amount keeps the order listed, the extra units
  are reported as strays and never count as liquidity (regression test `crates/kob-executor/tests/custody_exactness.rs`). `GET /v1/orders/{id}` returns `custody { expected_amount, utxo, ok }` and
  `strays`, each token UTXO with its proven `state` (the JSON the builders take; absent for a token the indexer does not track), and
  the order's `extension_commitment` (an if-done exit stores its entry's: the entry covenant writes it into the exit's custody), so a
  client can build the cancel, cancel-replace or refund of every kind from the view alone; the books list an ask only while exactly one live custody UTXO holds the expected
  amount; strays never count as liquidity; `GET /v1/strays` lists live ones (`lost: true` when the
  order has terminated and the maker's cancel can no longer sweep it: nothing can, no input will carry the id again). A
  token UTXO of ANY other token owned by an order id is a FOREIGN stray: flagged `foreign: true` with its proven `state` and
  `program` (the extractor reads the transfer leader of every token transfer that is not an order's own continuation to find
  them). A maker's SWEEP in place (`kob1-payload.md`) keeps the order live: event `sweep`, the same state at the new
  outpoint. Bid-side kinds own no tokens:
  every token owned by their id is a stray.
* **Auctions.** `GET /v1/orders/{id}` (and each order of an unaggregated book) carries `auction`
  with `kind` (`decay`, `rise`, `stop`, `entry`), `origin_daa`, `start_price`, `end_price`,
  `current_price` at the node's DAA score and whether it is `complete`; `quote` is the price now
  (`price` stays the limit). The fill event's `price` is the covenant's bound at the call's time
  argument `t`.
* **IOC / FOK.** `kill_daa` is the kill time (`max(UTXO DAA, activeFrom) + 600`, or the expiry if
  earlier). The unfilled rest of an IOC fill that returns in the same transaction, and a keeper's
  kill after the window, are `kill` events; the order's status is `killed`. A FOK fill of the whole
  order is `filled`.
* **Day orders.** The placement record's `deadline` (UTC unix seconds) is stored and returned with
  `deadline_passed`; it never changes what the covenant accepts (soft expiry).
* **Listing** (books) is policy on top of indexing: an order is `listed` only if its token is in the
  allowlist and it meets the listing rules (standard scale, minimum order value, 90-day expiry).
  Unlisted orders are still indexed and queryable by covenant id (makers can always find them).

#### Pair orders (`KobPair`, `KobCondPair`, `KobIfdPair`)

A pair order trades a base token A (the order's `token`) against a quote token B (`quote_cov_id`), each of either family, at a
price in **B base units per whole A** (`scale(A)` base units); its tips, keeper tips and carriers are KAS. One template per kind
serves both sides: `side` 1 (ASK) sells A for B, 2 (BID) buys A with B; a buy-first `KobIfdPair` is a BID, a sell-first one an
ASK. Option (2) of the founder rules: a pair order enforces only its own guarantees, so a filler may route it through the KAS books,
net it against opposite pair orders of the same pair, or fill it from inventory (Part C).

* **Discovery**: the placement record (payload v4 kinds `0x08` / `0x09` / `0x0a`), validated by `recover_orders` like any
  order. Its custody parts follow `AnyState::custodies`, each in its own token's family: a `KobPair` / `KobCondPair` holds
  ONE custody of the token it sells (S: A for an ASK, B for a BID's escrow); a buy-first `KobIfdPair` its B escrow; a
  sell-first one its A (exactly `amountLeft`) and its B **prefund** (the record's `prefund` part). Both are stored as custody
  rows of their own token. The `orders` row: `token_cov_id` A, `quote_cov_id` B, `side` from the state, `in_book` 0, `price`
  null (no KAS price), `tip` the KAS tip per whole A, `ext_commit` the first custody's extension commitment (an exit: its entry's
  `aExt` / `bExt`). The token registry records both identities.
* **Listing**: token A passes the rules of an ask (allowlist / registry and strict template list, family, extension commitment,
  standard scale, 90-day expiry, carrier; no KAS minimum order value); token B the same rules with its own scale, a refusal being
  `unlisted_reason = quote_<reason>`. Exits inherit their entry's decision. The numeric gate applies.
* **Exact custody**: an output owned by the order id is a custody only when it is exactly one of the custodies the order's state
  after the transaction holds (that token, that amount, plain, covenant-owned, the token's family, a KCC-20 extension commitment the
  order's custody of that token carried before or the state names for it) and the order has no other live custody of that token;
  anything else of EITHER pair token owned by the id is a stray of the order itself (never foreign).
* **Lineage**: continuations are proven by splicing the mutable windows (`KobPair`: `amountLeft`, `custody`; `KobCondPair`:
  `stopPrice`, `armed`, `amountLeft`, `custody`; `KobIfdPair`: `armed`, `amountLeft`, `custody`, `rptAmount`). The rests sit at
  their custody inputs' indices. `settle` / `fill` with `nb > 0` is a fill; with `nb < 0` a repeat merge; with `nb = 0` an
  update (`upd = 1`: arm or trail) or a refund / kill (`upd = 0`); `cancel` is the maker's cancel. A `KobIfdPair` fill creates a
  fresh `KobCondPair` exit (`IfdPairState::exit_for`, booked with `parent` / `rptPrice` / `rptPre` / `rptUntil` while
  `rptAmount > n`), whose custody is the leg's positional output (buy-first: n of A; sell-first: the proceeds plus the prefund of
  the fill, in B). A booked exit's take-profit merges its entry (`rearm` on the entry: its custodies grow, new ones at the
  covenant's `tokOut`; `merged_into` on the exit's fill).
* **Events**: `fill` / `kill` / `refund` / `cancel` / `arm` / `trail` / `rearm` as for the KAS kinds. A pair fill's event has NO
  price (`price` null, never a KAS trade); `detail.pair` carries `side` (`ask` / `bid`), `base`, `quote`, `a_scale`, `amount_a`,
  `amount_b` (B the maker received, an ask, or paid, a bid: the call's `tOut` / `sOut`, an entry's `amt`), `price` (the order's
  quote at the fill, B per whole A) with `price_num` / `price_den` (per A base unit), `tip_kas`, `counterparty` and
  `price_source: "none"`. `counterparty` is `route` when the transaction also fills KAS-book orders of A or B (those fills record
  their own KAS trades), `netting` when an opposite pair order of the same pair fills in it (and no KAS-book order of A or B),
  `inventory` otherwise. The arms, trails and triggered fills of pair conditionals carry `detail.evidence`: `mode` 0 (two KAS-book
  fills: `inputs` [A, B], `orders`, the quotes `a` and `b`) or 1 (a resting `KobPair` of the pair: `inputs` [k], `orders`, its
  `price`).
* **Price rule (founder)**: trades, candles, last prices and statistics are per token in KAS and come ONLY from KAS-book fills.
  Each pair fill is a `pair_fills` row (volume of the pair (A, B) in base units of both, the order's quote, the counterparty,
  `price_source` none; reverted with its block) and never a price. The pair charts are derived from the two KAS series (5.4).
* **Change feed**: a change of a pair order notifies `order:<id>`, `book:<A>` and `book:<B>` (clients refetch the pair views).

#### What is exact and what is estimated

| Field | Source | Exact? |
|---|---|---|
| order terms (price, scale, minimum fill, expiry, maker, token, deadline) | placement record, verified against the P2SH | exact |
| fill amount `n` (base units), refund / cancel / update / merge | the entry and arguments of the input's signature script | exact (verified spend) |
| maker payout of a sell-side fill | value of the output at the input's index (positional rule) | exact |
| cost of a buy-side fill | `detail.escrow_before` (the spent order output), `detail.escrow_after` (its continuation; absent when the bid sold out) and `detail.delivery_value` (the maker's delivery output at the input's position: carrier plus a rising bid's surplus); cost = before - after - delivery | exact |
| current state (`amountLeft`, `armed`, trailing `stopPrice`, `rptAmount`) | proven splice of the mutable windows | exact when proven, else `state_known = false` |
| remaining amount of asks, conditional orders and entries | `amountLeft` of the proven state | exact (`amount_estimated: false`) |
| remaining amount of a bid | its buying power: the largest `n` whose budget `ceil(n × (pMax + tip) / scale)` leaves `deliveryCarrier + reserve` of the escrow (`BidState::buying_power`), the most ONE fill can buy (every further fill moves one more carrier): an upper bound | estimate (`amount_estimated: true`) |
| `auction.current_price` | protocol arithmetic at the node's DAA score | exact for that score |
| custody / strays / holdings | leader `next_states`, verified against output scripts | exact |

The node reports `blockDaaScore: null` on every spent UTXO entry of a VSPC v2 response [F: TN10,
2026-09-29], so the indexer keeps the DAA score of the block that created each tracked UTXO itself
(needed for `armed`, `rptUntil`, auction origins and kill times).

#### Storage policy

A `High` VSPC response is about 18.2 KB per chain block on TN10, about **15.7 GB per day** at 10
BPS [F, section 3]: signature scripts carry whole KCC-20 programs. **None of it is stored.** What is
kept:

| Data | Kept | Where | Size |
|---|---|---|---|
| KOB1 genesis transactions (placement records), every transaction that touches a KOB covenant, token custody / stray outputs, transfers of allowlisted tokens (holdings), reorgs, operator operations | forever, as compact extracted records | record log (`records/seg-*.kobrec`) | about 1 KB per KOB transaction, about 1 to 2 KB per order lifecycle |
| Orders, UTXOs, events, custody, token registry events | forever, derived | `index.sqlite3` | about 2.6 KB per order |
| Chain-block rows (hash, DAA) | the reorg / finality window only (`reorg_window_hours`, 12) | `blocks` | 98 B per block, about 42 MB in steady state at 10 BPS |
| Everything else in the node's responses | never | | 0 |

The database is a *derived view* of the record log: `kob-executor index replay` rebuilds it from the
records and the follower re-syncs the tail from the node (section 7.6). Measured numbers are in
section 3; they come from `cargo test -p kob-executor --test storage -- --nocapture`.

**Token registry events.** The first sighting of a `(token covenant id, token program, extension
commitment)` identity is recorded as a `seen` event (`GET /v1/token-events`). It is derived from
placement records, so it is rebuilt from the log like everything else.

#### Storage: why SQLite (WAL)

The indexer needs one atomic commit of (cursor + order delta + undo stamps + record-log position),
ad-hoc queries for the API (book by token and price, orders by maker, recent fills), and files an
operator can inspect and back up. SQLite gives that with one small C dependency: `sqlite3
index.sqlite3` opens the live database, `VACUUM INTO` is a consistent online backup, WAL readers
never block the writer, and `synchronous=FULL` makes a committed batch survive power loss. RocksDB
(a key-value store) would need hand-built secondary indexes for every API query and a C++
toolchain; flat JSON files (the legacy scanner) are not atomic. The data is small (KOB orders, not
the chain), so SQLite is nowhere near a bottleneck; the follower's cost is the node's response time
and the link.

**Checkpoints.** The cursor (last applied chain block hash and DAA score) is stored in the same
transaction as every batch, so every commit is a checkpoint. Undo data is not a separate log: every
row carries the chain block that created or spent it (`seq`, never reused), and reverting a block
deletes its rows, un-spends what it spent and re-derives the affected orders. A block row is deleted
once it is older than the reorg window; the rows it stamped are final (12 h is consensus finality).

### 2. Node

Run a dedicated node per indexer (do not share the keeper's node with public API load):

```
kaspad --utxoindex --retention-period-days=7 --disable-upnp \
       --rpclisten-json=127.0.0.1:18210 --maxinpeers=32
# testnet-10: add --testnet --netsuffix=10
```

* `--retention-period-days=7` (N >= 2) keeps acceptance data and transaction bodies for 7 days
  without `--archival` [F: `kaspad/src/daemon.rs`]. Without it the node prunes after about 30 h
  [F]. Because the indexer keeps its own extracted records, the node's retention is only the
  **downtime the indexer may survive**: after an outage (or after a rebuild from the record log) the
  follower resumes from the stored cursor and needs the node to still have the chain from there
  (section 7.4). Keep it at 7 days, and at least 2; a shorter retention only shortens the
  survivable outage, it never loses recorded orders. Do **not** run `--archival` for the indexer.
* `--utxoindex` is needed only for the recovery tools (`getUtxosByAddresses`). The follower itself
  does not need it; enabling it costs disk and CPU [U: measure at 10 BPS].
* `--unsaferpc` is **not** needed and must not be set: VSPC v2, `getUtxosByAddresses`,
  `getBlock` and `getBlockDagInfo` all work in safe mode [F]. CI should grep the unit file.
* wRPC JSON is plain `ws://`; keep it on loopback or a private network and never expose it.
  Firewall everything except P2P 16111.
* The node DB upgrade is one-way and a missed fork splits the node: upgrade TN10 first, then the
  standby, then the primary. `getServerInfo.serverVersion` is exposed in `/v1/health`
  (`node_version`); alert when it trails the newest release.
* Official minimum hardware for a node: 8 cores, 16 GB RAM, 640 GB SSD [F]. With 7-day retention
  size the disk from the node's formula, worst case about 1.6 TB if every block were full [I]. The
  indexer's own disk is small (section 3); the node is the big consumer.

### 3. Resources

| Component | Estimate | Basis |
|---|---|---|
| Indexer RSS | 0.3 to 1 GB typical; a full-size `High` batch is parsed in memory, budget up to 4 GB on mainnet [I]. While catching up with parallel fetch, up to `prefetch_max_mb` (default 128 MiB, wire bytes, a hard bound; about 3x that in RSS at the peak) of windows ahead of the cursor on top; the writer keeps 64 MiB of SQLite page cache | one TN10 batch of 1,716 chain blocks is 31.2 MB of JSON (see below) [F]; 259 MB held ahead at a 256 MiB budget on TN10, 341 MB before the bound was hard [F] |
| Message size cap | 1 GiB per response (`WrpcConfig::max_message_bytes`) | full blocks are 250 KB x up to 2,480 blocks in the worst case |
| CPU | one core at most (the write path is single-threaded): **10 to 12 us per accepted transaction** of a realistic mix (1.2 % KOB), 2 to 4 % of one core at 3,000 tx/s; about **0.25 to 0.3 ms per KOB transaction** (3,500 KOB tx/s per core at the tip); see *Processing capacity* below | [F] `capacity` benchmark |
| Chain-block rows | 98 B per block; 12 h at 10 BPS is 432,000 blocks, **about 42 MB**, constant (older rows are deleted) | [F] `storage` test |
| Record log | about **1 KB per KOB transaction** (982 B average over the lifecycles below), **1 to 2 KB per order lifecycle**; 8.5 % of the raw JSON of the same transactions; plus one checkpoint frame per hour (about 110 B, 2.6 KB per day) | [F] `storage` test |
| Derived SQLite database | about **2.6 KB per order** (300 orders, 450 transactions: 786 KB), plus the block rows above | [F] `storage` test |
| Traffic from the node | **about 1.35 KB of JSON per accepted transaction** at `High` (1.35 MB/s at 1,000 tx/s), none of it stored; quiet TN10 was about 180 KB/s (15.7 GB/day). Run the node and the indexer on the same host (loopback) or LAN: see *Bandwidth* below | [F] TN10 measurements below |

**Storage per day on TN10** (2026-09-29). The provisional design retained the raw responses (15.7 GB/day
at `High`); the storage policy below removed that. Now: TN10 carries no
KOB1 payloads and no KOB orders yet (a live sample of 941 chain blocks, 8,611 transactions, 83
covenant outputs from other projects, produced no order and no reject; the live follower test, which
uses the production write path, followed 1,289 chain blocks including several real reorgs and left a
record log of 114 bytes, 1,279 chain-block rows and a 300 KB database), so the permanent growth is
the hourly checkpoint frames, **about 2.6 KB per day**, on top of the constant 42 MB block window. Every
order adds about 1 to 2 KB to the log and 2.6 KB to the database; a TN10 day with 1,000 orders
placed and partly filled would add about 1.5 MB to the log and 2.6 MB to the database. The savings
compared with keeping the response are three to four orders of magnitude.

**Storage per order** (real v2.4 transactions built with the protocol builders, records versus the
raw wire JSON of the same transactions):

| Lifecycle | txs | record log | raw JSON | of which signature scripts | log / raw |
|---|---|---|---|---|---|
| limit ask: create + partial fill + cancel | 3 | 2,419 B | 33,329 B | 13,728 B | 7.3 % |
| limit bid: create + partial fill | 2 | 1,472 B | 13,357 B | 5,017 B | 11.0 % |
| repeat IFD: create + fill (booked exit) + take-profit merge | 3 | 3,967 B | 45,667 B | 19,347 B | 8.7 % |

A create transaction costs its placement payload (323 to 683 bytes, `kob1-payload.md`) plus the
outpoints and outputs; a fill costs the revealed state span (243 to 603 bytes) plus arguments. The
signature scripts, redeem scripts, token programs and every non-KOB transaction are dropped.

**TN10 measurement** (2026-09-29, a project-operated v2.1.0 TN10 node, over a WAN link with about 300 ms RTT, one
connection) [F]: `getVirtualChainFromBlockV2` from 1,716 chain blocks behind the sink returns 31.2 MB at
`High` (37 s), 25.7 MB at `Low` (18 s) and 14.4 MB at `None` (7 s). `High` is only 21 % larger than
`Low`, so nothing is gained from a lighter level. In a second sample 852 chain blocks carried 8,182
transactions (118 covenant outputs, all from other projects) and every input carried its spent UTXO
(with `blockDaaScore: null`). One call returns at most about 2,480 chain blocks, and fewer when the batch
is large. At about 10 chain blocks/s, a follower one hour behind needs 15 to 20 calls and one 30 hours
behind about 450. Catch-up over the WAN ran at 18 to 50 chain blocks/s in two runs (2 to 5 times real
time), dominated by the fetch: for a 997-block, 9,890-transaction batch the follower measured fetch 6.2
s, parse 48 ms and database commit 15 ms. Local processing is therefore not the bottleneck; the node's
response time and the link are, so co-locating node and indexer matters [U: measure on loopback]. The
follower logs `fetch_ms`, `parse_ms` and `commit_ms` for every batch of 100 or more chain blocks.

Still to measure on the TN10 soak and mainnet: RSS at `High` under a flood, record-log growth per day under real KOB
traffic, and the effective pruning cliff [U].

#### Bandwidth: what following the chain costs per transaction, and why the indexer runs next to its node

The follower's traffic scales with the chain's **accepted transactions**, not with KOB's: every accepted transaction
arrives in full, whether it touches KOB or not. Measured on TN10 on 2026-10-02 during the flood (a v2.1.0 node over the
WAN, one bounded batch of 71 chain blocks and 8,661 accepted transactions, mostly transfers with ~200-byte payloads) [F]:

| Verbosity | bytes per accepted transaction | carries |
|---|---|---|
| `Low` (the standalone matcher's acceptance follower) | 1,182 | ids, outputs, signature scripts; **no previous outpoints, no payloads** |
| `High` (the indexer) | 1,354 | + previous outpoints, spent UTXO entries, payloads |
| `Full` | 1,354 | same as `High` on v2.1.0 |

`/v1/health` reports the running figure as `bytes_per_tx` (`wire_bytes_total / txs_fetched_total`, also
`kob_indexer_bytes_per_tx` in `/v1/metrics`). Bandwidth needed per indexer, at 1.35 KB per transaction:

| chain load | to follow | to catch up 1 h of lag in 1 h (2x) | in 15 min (5x) |
|---|---|---|---|
| 100 tx/s | 135 KB/s (1.1 Mbit/s) | 270 KB/s | 0.7 MB/s |
| 1,000 tx/s (the 10-01 flood) | 1.35 MB/s (11 Mbit/s) | 2.7 MB/s | 6.8 MB/s |
| 3,000 tx/s | 4.1 MB/s (32 Mbit/s) | 8.1 MB/s | 20 MB/s |

A follower on a link of bandwidth `B` behind a chain producing `R` bytes/s catches up at `B / R - 1` times real time, and
never when `B <= R`. That is what happened on 10-01: the soak PC gets 1.2 to 1.8 MB/s from the remote node, the flood
produced about 1.35 MB/s per indexer and two indexers shared the link, so a follower that fell behind could not catch up
even with bounded batches. **Production indexers run on the node's host (loopback) or on its LAN**; a remote node is for
development only. Give the link at least 3x the chain's peak byte rate.

**Cheaper request shapes (none available on v2.1.0)** [F: `rpc/service/src/service.rs`,
`rpc/core/src/convert/verbosity.rs`]: `getVirtualChainFromBlockV2` takes a start hash, one verbosity level and
`minConfirmationCount`; it has no filter by script, covenant or payload prefix, and no size or count limit. `High` is
the lowest level that carries what the indexer needs (previous outpoints to see a tracked output spent, payloads for
KOB1 records); `Low` saves 13 % and lacks both. Acceptance data without blocks is what VSPC v2 already is (the merged
blocks themselves are never fetched). Fetching only KOB transactions would need either a node-side filter (a node
change: outputs by covenant id or KOB script template, inputs by tracked outpoint, payloads by `KOB1`) or a two-pass
scheme (ids at `Low`, then the KOB transactions by id), which the node cannot serve without a transaction index. The
binary encoding (Borsh wRPC) drops the hex doubling and the field names: see *Borsh windows* below (`borsh = true`, about
2.5x fewer bytes). Co-location stays the answer for production.

#### Borsh windows: the same answers in a third of the bytes

With `borsh = true` (`--borsh`) the windows of transaction bodies (`getVirtualChainFromBlockV2`) and the primary's
accepted transaction ids (`getVirtualChainFromBlock` with ids) travel over the node's **Borsh wRPC** endpoint instead of
JSON (`rpc::borsh`). Every other call (status, blue scores, chain hashes, submissions) stays JSON on the configured URL.
The bodies are decoded with the node's own RPC types (`kaspa-rpc-core`, the release the node runs) and converted to the
same wire types as the JSON path, so the follower, the checks of other nodes (`rpc::verify`) and the extractor see
identical data (a unit test decodes one answer both ways; `tests/tn10_borsh.rs` compares live JSON and Borsh windows and
verifies a Borsh `Full` window from a public node against the primary's ids over Borsh). Message size limits, timeouts,
the size accounting of the prefetch and the multi-node checks are unchanged; `wire_bytes` and `bytes_per_tx` are Borsh
bytes then.

Measured on TN10 on 10-07 during a flood of plain transfers (2,200 accepted tx/s, a v2.1.0 node over the WAN, one window
each) [F]:

| encoding, verbosity | bytes per accepted transaction |
|---|---|
| JSON `High` | 1,423 |
| Borsh `High` | 577 |
| Borsh `Low` | 464 |
| Borsh `Full` | 594 |

Transactions with many inputs gain more (an input is ~660 bytes in JSON, ~230 in Borsh [U: estimated from the encoding]). `Low` is not worth a second pass
in Borsh (20 % less, and it still lacks outpoints and payloads). On 10-07 the soak PC's link carried about 6.5 MB/s in all;
the flood produced about 7 MB/s of JSON per indexer, so neither of two JSON indexers could keep up, whatever the
connections and the prefetch budget; in Borsh the same chain is under 3 MB/s per indexer.

Endpoints: a node's Borsh URL is derived from its JSON URL (a path ending in `/json` becomes `/borsh`:
`wss://<node>/kaspa/testnet-10/wrpc/borsh` on the public nodes; else the default JSON port `18xxx` becomes the Borsh
port `17xxx`: `ws://<host>:17210` on TN10, `17110` on mainnet, which the node opens with `--rpclisten-borsh`); a
`[[nodes]]` entry's `borsh_url` overrides it (also for the primary: an entry with `role = "primary"`). A node with no
Borsh endpoint known stays JSON (logged at start). Borsh needs VSPC connections of their own (`fetch_parallel` above 1);
with `fetch_parallel = 1` everything stays JSON.

#### Batch size under load (the 10-01 livelock)

The node bounds a VSPC response only by chain blocks (`mergeset_size_limit * 10`, 2,480 on TN10) and merged blocks,
never by bytes. Under the 10-01 flood a follower that fell behind asked for an ever larger batch (up to 198k
transactions, about 300 MB) until none arrived within `rpc_timeout_secs`, and the old follower retried the same request
forever (60 identical timeouts, cursor frozen for 3.4 h). The follower now bounds every request to a window of **blue
score** ahead of its cursor through `minConfirmationCount`: the node keeps an added chain block only while
`sink_blue - block_blue > minConfirmationCount`, so `minConfirmationCount = sink_blue - (cursor_blue + window)` returns
the chain blocks with blue score in `(cursor_blue, cursor_blue + window)` (about one block per blue score at 10 BPS).
The window adapts (`rpc::window::BatchWindow`):

* it starts at 600 blue score and aims for one batch per `rpc_timeout_secs / 6` (30 s at the default 180 s): it halves
  after a slower batch and doubles after one faster than half of that;
* a batch that **times out is split**: the next request asks for a quarter of it (never below a window that came back
  empty from the same cursor; between the two it bisects). When the window cannot shrink further (the next single chain
  block is larger than the link carries within one timeout) the request timeout doubles instead, up to 8x. A timed-out
  request is never sent again unchanged; `vspc_timeouts_total` counts the splits;
* a bounded request that returns no chain block (the next one lies beyond the window) widens the window and asks again
  at once;
* near the sink (lag below the window) the request is unbounded, as before.

A bounded batch leaves the state `catching_up`, so `caught_up` stays false and the matcher, the keepers and the
maintenance jobs plan nothing (`the book lags the node`) until the cursor is within `max_lag_secs` (default 30 s) of the sink
(`within_lag_tolerance`; they plan against that slightly old book) or reaches it. The window, the lag in blue
score, the last batch (blocks, transactions, bytes, fetch time) and the timeouts are in `/v1/health`
(`batch_window_blue`, `lag_blue`, `last_batch_blocks`, `last_batch_txs`, `last_batch_bytes`, `last_fetch_ms`,
`vspc_timeouts_total`, `consecutive_failures`) and in `GET /v1/metrics` (Prometheus text; `kob-executor run` also
appends them to its `--metrics-file`). Every bounded batch, and every batch of 100 or more chain blocks, is logged with
its bytes and window. A bounded request costs the node one block read per stripped chain block, which happens only
while the follower is behind. The standalone matcher's acceptance follower (`match` without `run`) bounds its `Low`
requests the same way.

#### Parallel fetch: catching up over several connections

One WAN connection carries far less than the link: TCP over a long round trip is window-bound, and every VSPC request
also costs the node a fixed amount of work (it walks up to 2,480 chain blocks and reads one block per chain block it
strips for `minConfirmationCount`, `rpc/service/src/service.rs`). Measured from the reference PC (Japan) to the TN10 node
(Germany, ~250 ms RTT) on 10-02: one HTTP download from a server in the same data centre ran at **1.0 MB/s**, four at once
at about **4 MB/s** together [F]. So while it is far behind, the follower fetches several windows at once
(`indexer::prefetch`):

* it asks the node for the selected chain as **hashes only** (`getVirtualChainFromBlock` without transaction ids: up to
  2,480 chain blocks for about 170 KB) and cuts it into windows of whole chain blocks `(start, end]`;
* each window is one `getVirtualChainFromBlockV2(start, High)`, bounded through `minConfirmationCount` to end at `end`,
  on its **own connection** (`fetch_parallel` connections for VSPC plus one for the small calls, so a status call never
  waits behind a batch); the windows are **applied strictly in chain order**, each in its own commit, while the next ones
  are still arriving;
* a window is used only if it continues the planned chain exactly (no removed blocks, its added hashes a prefix of the
  plan). A **reorg** reported by any window or by the next hash list drops every window that holds or starts after a
  removed block, fetched or not, and keeps the ones before the fork; the follower then takes one ordinary step from its
  cursor, which handles the reorg as before. Windows already applied when the chain moves are reverted by that step like
  any batch (`prefetch_discards_total` counts the dropped plans). A window the node cut short (merged-block limit) is
  applied and the rest fetched again;
* per window, the batch rules above apply in chain blocks: a window that times out is split in half (a single chain block
  gets a longer timeout instead, up to 8x), the window size halves after a fetch slower than its target and doubles after
  a fast one; with windows sharing the link the target is `rpc_timeout_secs / 6` times `min(fetch_parallel, 3)`;
* memory is bounded by `prefetch_max_mb` (default 128 MiB), **hard**: windows fetched and not yet applied count with their
  wire size, windows in flight with the **allowance** they were sent with, and a window is sent only while the sum stays
  within the budget. The allowance is the window's estimate (wire bytes per chain block, plus a quarter) and is also the
  request's own size limit: the websocket client refuses a larger answer as its frame header arrives, before buffering the
  payload (an answer from a source without that check is dropped the same way). An oversized window raises the estimate
  per chain block to what it reported at once (the windows after it are smaller), and is split in half, or, a single chain
  block, sent again with an allowance of its reported size (`prefetch_oversize_total` counts them). The window at the
  cursor always goes: with the room the budget leaves, and when that is too little the windows behind it are dropped
  (fetched again later) so it gets the whole budget. The one excess left: a single chain block larger than the whole
  budget is fetched alone, with nothing else held. The window size is capped so that `fetch_parallel` windows fit;
* near the sink (lag at most `prefetch_min_lag_blue`, default 1,200 blue score, about two minutes) the follower is back to
  single steps. One step is one connection: under a flood that one connection cannot carry (on 10-07 in the soak, about
  1.2 MB/s per connection against 1.5 to 1.8 MB/s of Borsh bodies per second of chain), the lag settles near the threshold
  instead of reaching the tip; `prefetch_min_lag_blue = 30` keeps the windows parallel to within a few seconds of it.

Config: `fetch_parallel` (default **4**, 1 turns it off), `prefetch_max_mb` (default 128; 256 until the hard bound),
`prefetch_min_lag_blue` (default 1,200),
`borsh` (the windows over Borsh wRPC, *Borsh windows* above). Health: `prefetch_windows`,
`prefetch_in_flight`, `prefetch_bytes`, `prefetch_bytes_peak`, `prefetch_window_blocks`, `prefetch_discards_total`,
`prefetch_oversize_total`.

**Why the bound is hard now.** On 10-02 the soak's two indexers caught up through the tail of the TN10 flood with the
estimate-based bound: their peaks were **341 MB and 327 MB against the 256 MiB (268 MB) budget**, 27 % over, because the
estimate for windows in flight was the average size per chain block so far and the flood's chain blocks were 10x the calm
ones (RSS 955 and 769 MB; `tools/soak/README.md`). The quarter of margin could not cover that. With the allowance as the
request's size limit no answer can exceed its share, and `tests/flow_follower.rs`
(`prefetch_holds_its_budget_when_windows_come_back_two_to_three_times_larger_than_estimated`) holds the peak within the budget
when the chain blocks grow 2x and 3x after the estimate was learnt, with the transport's check and without it. The process
holds about three times the budget at the peak (the parsed windows next to their JSON), so the default came down to 128 MiB
(two indexers on one small host stay near 0.5 GB each); take 256 or more on a dedicated host with a fast link.

**Measured** (`tests/tn10_capacity.rs`, read only: the follower starts 4 to 5 hours behind the TN10 sink at the same chain
block in every run, production write path, 150 s per run, default budget; the remote node above; two soak indexers
followed the same node over the same link meanwhile) [F]:

| `fetch_parallel` | run 1 (10-02 ~06:45 UTC) | run 2 (~07:25 UTC) | run 3 (~07:45 UTC, final code) |
|---|---|---|---|
| 1 | 2.22 MB/s, 1,582 tx/s, 4.4x real time | 0.82 MB/s, 582 tx/s, 1.6x | 1.21 MB/s, 867 tx/s, 2.4x |
| 2 | 2.41 MB/s, 1,697 tx/s, 4.8x | | |
| 4 | **3.85 MB/s, 2,709 tx/s, 7.5x** | 1.88 MB/s, 1,335 tx/s, 3.7x | 2.79 MB/s, 1,977 tx/s, 5.5x |
| 8 | 3.32 MB/s, 2,280 tx/s, 6.4x | 2.94 MB/s, 2,074 tx/s, 5.8x | 2.71 MB/s, 1,905 tx/s, 5.3x |

The link changes by the hour (one connection: 2.2 then 0.8 MB/s), so compare within a run: 4 windows gave 1.7x, 2.3x and
2.3x of one, 8 gave 1.5x, 3.6x and 2.2x. The best run reached the link's own aggregate (about 4 MB/s): at 1.4 KB per accepted
transaction a remote indexer on this link follows at most about 2,700 tx/s, short of a 3,000 tx/s chain. No window timed
out. The most held ahead was 262 to 292 MB at the 256 MiB (268 MB) budget before the estimate margin was added, 267.9 MB
after it (run 3). Run 1 used a fixed 30 s window target, run 2 the scaled target without the margin, run 3 the final
code.

**Recommendations.**

* **Co-located node (loopback or LAN, the production setup):** `fetch_parallel = 2` to `4`. The link is not the limit
  there; parallel windows overlap the node's per-request work with the transfer and the indexer's commit. The indexer
  processes a realistic mix at about 100,000 tx/s on one core (below), so catch-up is bounded by the node [U: not
  measured on loopback].
* **Remote node (development, WAN):** `fetch_parallel = 4` (default) to `8`; more than 8 only adds windows that wait.
  Each extra window costs one connection on the node and up to `prefetch_max_mb / fetch_parallel` of memory. Even then a
  WAN link of a few MB/s cannot follow a 3,000 tx/s chain: give the link at least 3x the chain's byte rate (section
  *Bandwidth*).
* `prefetch_max_mb`: the default holds about 4 windows of 30 to 60 s of TN10 flood chain; raise it only with
  `fetch_parallel` above 4 on a fast link and memory to spare (about 3x the budget at the peak), lower it (64) on very
  small hosts. A smaller budget only means smaller windows (fewer chain blocks per request), not a slower link.

#### Several nodes: bodies from every node, the chain from one

One node over a WAN link is limited by its link and by its own load (on 10-04 the project's TN10 node, serving the soak's
two indexers, gave 0.11 MB/s on one connection while public TN10 nodes gave 1 to 2 MB/s each, see below). The indexer can
therefore read the transaction bodies of the parallel fetch from several nodes at once while one node, the **primary**,
stays the authority for the selected chain (`rpc::multi`):

* everything the follower decides on comes from the primary: the cursor's position, the chain hashes the windows are
  planned on, every single step near the sink, every reorg. The other nodes only ever serve windows `(start, end]` of a
  chain the primary planned;
* a window goes to the node with a free connection that delivered the most bytes per second lately (a node not measured
  yet counts as fast; a node below a quarter of the best rate gets windows only when no faster one can take it, since in
  chain order a slow window holds up every window behind it; an idle node is measured again after five minutes);
* a window from another node is asked for at `Full` verbosity (the whole header and every transaction field, about 10 %
  more bytes than `High`) and **checked before the follower sees it** (`rpc::verify`), against the primary's acceptance
  data for the same start (`getVirtualChainFromBlock` with transaction ids, bounded to the window like the VSPC request,
  about 4 % of the bodies' bytes, fetched from the primary at the same time on connections of its own):
  * its chain blocks are the primary's, with no removed block (otherwise the node is behind or on another side of a reorg:
    *out of sync*, set aside for 10 s);
  * every header hashes to the block hash it was asked for (DAA score, blue score and timestamp are the real ones);
  * every chain block accepted exactly the transactions the primary reports, in the same order;
  * every transaction hashes to its id (outpoints, outputs with covenant bindings, payload, version, lock time);
* two parts of a transaction are outside every hash: its signature scripts and the spent outputs the node attaches to its
  inputs. The indexer reads both for the transactions it tracks, so before a window from another node is applied, every
  chain block holding a transaction that can matter to KOB is fetched from the primary and must agree with it in
  everything the indexer reads (`indexer::trust`). A transaction can matter when its payload is a KOB1 record, an input
  spends an output the store tracks (or an output of such a transaction earlier in the window), or an output carries the
  binding of a token some order trades or the allowlist tracks: all decided by the hashed parts and the store, never by
  what the other node says alone. On TN10 that is a few percent of the chain blocks;
* a node that contradicts a hash or the primary is **dropped** until the process restarts (logged as an error, `lies` in
  its health); its window goes to the next node. A node that fails a window otherwise (transport, timeout, it does not know
  the window yet) backs off (5 s, doubling to 2 minutes) and the window goes to the next node, the primary last;
* transactions of the executor (the matcher, the keepers) are submitted to every node that takes submissions at once; the
  first node that takes one answers, the others go on for propagation. When none takes it, the primary's rejection counts
  (it decides double spends and missing inputs), unless the primary failed on the wire and another node gave a verdict.
  The x402 facilitator still submits to the primary only.

What a lying node can still do: hide a token transfer of a token nobody trades sent to an order id (a "foreign stray", which
the indexer only flags for information), and slow the indexer down until it is dropped. It cannot add, remove or alter an
accepted transaction, a chain block or anything of a transaction the indexer tracks.

Config (`rpc_url` stays the primary unless an entry says `role = "primary"`):

```toml
rpc_url = "ws://127.0.0.1:18210"   # the primary: chain authority (and a body node unless primary_fetch = false)
primary_fetch = true               # false: the primary serves windows only when no other node can (it stays the authority)
[[nodes]]
url = "wss://boson-10.kaspa.red/kaspa/testnet-10/wrpc/json"
# role = "secondary"   fetch = true   submit = true   connections = <fetch_parallel>
[[nodes]]
url = "wss://alpha-10.kaspa.stream/kaspa/testnet-10/wrpc/json"
```

or on the command line `--node <url>` (repeatable) and `--no-primary-fetch`. `ws://` and `wss://` (rustls, webpki roots)
both work. The windows in flight are `fetch_parallel` on the primary plus each other node's `connections`, sharing one
`prefetch_max_mb`. Public TN10 nodes: the Kaspa resolver (`https://<resolver>/v2/kaspa/testnet-10/tls/wrpc/borsh`, the
resolvers of rusty-kaspa's `rpc/wrpc/client/Resolvers.toml`) hands out `wss://<node>/kaspa/testnet-10/wrpc/borsh`; the same
node serves JSON at `.../wrpc/json`.

Health: `nodes` lists every node (`role`, `state` `ok` / `backoff` / `dropped`, windows, chain blocks, transactions, bytes,
`bytes_per_sec`, timeouts, `out_of_sync`, `lies`, `dropped_reason`, `submits_ok` / `submits_failed`), plus
`untrusted_windows_total`, `untrusted_blocks_total`, `confirmed_blocks_total` (chain blocks compared with the primary's
copy) and `lies_total`; metrics `kob_indexer_node_*{node=...}`.

#### Processing capacity: CPU per transaction (the 3,000 tx/s benchmark)

`tests/capacity.rs` generates accepted-chain data at 10 BPS with 300 transactions per block (3,000 tx/s) and feeds it to
the processing pipeline without a network: the JSON of a `getVirtualChainFromBlockV2` response at `High` (non-KOB
transactions with exactly the field set of a captured TN10 transfer, about 1,380 B each), parsed as the client parses it,
checked and committed (`Ingest::apply_batch`: file database in WAL mode with `synchronous = FULL`, record log with
fsync). KOB traffic is real: asks placed, filled in part (4 of 10) and cancelled with the protocol builders, and x402-style
token payments of an allowlisted token whose ids the facilitator watches. 60 s of chain per run, generation not timed.
Reference PC: **AMD Ryzen 7 5800HS (8 cores, 16 threads), 15.4 GB RAM, NVMe SSD, Windows 11, release build**, measured on
2026-10-03 while the same PC ran a TN10 node, a CPU miner and the soak (so these are lower bounds) [F]:

| mix (per block of 300) | follow the tip (5 blocks per commit) | catch up (100 blocks per commit) | CPU at 3,000 tx/s | disk writes |
|---|---|---|---|---|
| no KOB (5 % foreign covenant outputs) | 259,000 tx/s, 3.6 us/tx | 301,000 tx/s, 3.2 us/tx | 1 % of one core | block rows only |
| default: 1 placement, 1 fill, 0.5 cancel, 1 payment (1.2 % KOB) | **103,500 tx/s**, 10.2 us/tx (CPU) | **170,300 tx/s**, 6.4 us/tx | 2 to 3 % | 164 KB/s (DB 8.3 MB + log 1.5 MB per minute) |
| busy: 10, 10, 5, 5 (10 % KOB) | 21,400 tx/s, 40 us/tx | 35,800 tx/s, 30 us/tx | 9 to 12 % | 1.0 MB/s |
| KOB only: 100, 100, 50, 50 (10 s of chain) | **3,490 tx/s**, 287 us/tx | **4,290 tx/s**, 254 us/tx | 76 to 86 % | 10 to 15 MB/s |

Before 2026-10-03 the KOB-only mix ran at 1,700 to 2,100 tx/s (546 us per transaction): the write path runs about 60
distinct SQL statements per KOB transaction cycle and rusqlite's statement cache held 16, so nearly every execution
re-parsed and re-planned its statement (SQLite's own step time was 20 % of the total). Every connection now keeps 256
prepared statements. The pure work of a batch (signature-script parsing, script hashing, placement verification:
`indexer::record::precompute`, about 35 us per KOB transaction) runs on `verify_threads` threads (default: the cores, at
most 4) before the single-threaded database pass, which consults its results; `capacity.rs` checks that both paths give the
same rows. A KOB-only chain of 3,000 tx/s is followed at the tip with 1.2x headroom on one core of this (busy) PC.

Parsing costs 2.6 to 3.2 us per transaction (about 500 MB/s of JSON); a non-KOB transaction is dropped by the extractor
before any database work (no `KOB1` payload, no covenant input or output). A KOB transaction costs about **0.25 to 0.3 ms** of
one core: state derivation and its rows (the placement verification and script hashing run before, on other cores), in
no single dominant step. The database pass is single-threaded: its ceiling is about 3,500 KOB tx/s at the tip and 4,300
while catching up on this PC; at 10 % KOB a 3,000 tx/s chain is followed with 7x headroom at the tip.
Memory: 32 MB peak at the tip, about 200 MB while committing 100-block windows (one 40 MB response parsed).

Fixed by this benchmark: the per-order fill sum in `refresh_order_state` (run after every placement and fill) was planned
through the market index `order_events (kind, token, ts)` and read every fill of the chain: per KOB transaction it grew
from 0.5 ms after 5 s to 2.9 ms after 60 s of the busy mix (3,400 tx/s). It now goes through the order's own events
(`+kind`, a unit test pins the plans of the per-order queries), and the writer keeps 64 MiB of page cache instead of
2 MiB; the busy mix runs at 14,600 to 20,300 tx/s after 60 s.

**Per tx/s of chain load**, at `High`: **1.4 KB/s of link** (1,400 to 1,454 B per accepted transaction on TN10 on 10-02),
**3 to 10 us of one core** for a realistic mix plus **0.25 to 0.3 ms per KOB transaction**, and about 1 KB of disk per KOB
transaction (record log plus rows; non-KOB transactions write nothing but the per-block row). At 3,000 tx/s: 4.2 MB/s
(34 Mbit/s) of link and 3 to 4 % of one core for the default mix.

### 4. Running it

```
kob-executor index \
  --network testnet-10 --rpc-url ws://127.0.0.1:18210 \
  --data-dir /var/lib/kob-index --tokens /etc/kob/tokens.json \
  --listen 127.0.0.1:8090
```

`--config file.toml` holds the same settings (flags override the file). Every key is optional:

```toml
network = "testnet-10"
rpc_url = "ws://127.0.0.1:18210"
data_dir = "/var/lib/kob-index"
start = "pruning_point"          # or "sink", or { hash = "<chain block hash>" }
poll_interval_ms = 500
rpc_timeout_secs = 180
fetch_parallel = 4               # VSPC windows fetched at once while far behind (1: one at a time); section 3, *Parallel fetch*
primary_fetch = true             # with [[nodes]]: the primary (rpc_url) serves windows too; section 3, *Several nodes*
verify_threads = 0               # threads of the pure per-batch work before the single database pass (0: cores, at most 4)
prefetch_max_mb = 128            # memory budget (hard) of the windows held ahead of the cursor; the process holds ~3x at the peak
max_lag_secs = 30                # `kob-executor run` plans while the store is caught up or at most this many seconds behind (0: only caught up); A.1
settle_depth_daa = 100           # "settled" = accepted + N DAA (about 10 s at 10 BPS)
reorg_window_hours = 12          # chain-block rows older than this are deleted (consensus finality)
checkpoint_secs = 3600           # log an otherwise empty batch when the cursor moved this far
tokens_path = "/etc/kob/tokens.json"

[rules]                          # listing policy
require_allowlist = true
min_order_value_sompi = 100000000  # 1 KAS: an order's amount at its quote (a bid: its escrow)
max_expiry_span_daa = 77760000   # 90 days at 10 BPS
expiry_slack_daa = 864000

[records]                        # the permanent record log (formerly [rawlog], still accepted)
enabled = true
segment_bytes = 268435456

[api]
listen = "127.0.0.1:8090"
trusted_proxies = ["10.0.0.0/8"]        # your reverse proxy / CDN egress ranges
client_ip_header = "x-forwarded-for"    # or "cf-connecting-ip", "x-real-ip"
[api.rate_limit]
per_ip_rps = 20.0
per_ip_burst = 60
global_rps = 500.0
global_burst = 1000
```

Flags of note: `--reorg-window-hours` (default 12, consensus finality), `--no-record-log` (disables
the permanent record log: then a lost or outdated database can only be rebuilt from what the node
still retains; not recommended). Subcommands: `run` (default; the indexer alone), `replay`, `export-orders`,
`import-orders`, `rebase`.
`kob-executor run` (Part A) embeds this indexer together with the matcher and the keepers and takes
the same options.

Fresh database: it starts at `start` (default: the node's pruning point, i.e. everything the node
still retains). `--start sink` skips history (only new orders, plus imports). Run it under systemd
with `Restart=on-failure`; state is durable across restarts. The template versions are the ones
`kob-protocol` pins (there is no template directory any more).

#### Token allowlist

`--tokens registry/tokens.json` (a `deploy-mainnet` build without `--tokens` uses the registry it embeds and pins, A.3). The file is either the token registry itself (`schema_version` +
`templates`; it is validated by `kob_protocol::registry` and only its `listed` tokens enter the list, with
their template hash, family, extension commitment and `decimals`) or a bare array / `{"tokens": [...]}` with
snake_case or camelCase keys, where only the covenant id is mandatory. Optional `templateHash`,
`extensionCommitment`, `family` and `decimals` tighten the rule for that token (unknown keys are ignored).

**Strict templates, open tokens.** A registry document also carries the STRICT template list (its `reviewed` templates; `registry/README.md`;
no template is `reviewed` yet in the shipped `registry/tokens.json`, so it lists nothing until one is). With one loaded, the list is OPEN: every order whose token program (template hash and
family) is on the strict list is listed, whether or not its token has an entry (a lookalike ticker is harmless: identity is the
covenant id plus template hash); an order on any other program is unlisted with `token_template_not_strict`, a delisted token
with `token_delisted`. A registry entry adds the ticker, the decimals (the standard scale, enforced only for entries that set
`decimals`) and the standing: `official` (confirmed genuine) or `unverified`. Every token the executor lists is tracked
(custody, holdings). A bare list without templates keeps the old behaviour (only its entries are listed); without `--tokens`
nothing is listed. Third-party KCC-20-compatible programs (KaspaCom's KCC20 0.2.5) are accepted by adding their template to the
registry, never by code.

**Possibly frozen.** KOB has no freeze / seize mechanism: a token program that freezes a balance or blacklists an address just
rejects the transaction. The matcher pre-executes every transaction in the rusty-kaspa engine; when the TOKEN program rejects a
plan, the order it is attributed to (its custody input failed, or dropping it made the plan pass) is flagged in the store
(`order_flags`, per order UTXO, so the flag lapses when the order moves), left out of the books, depth and counts, and
returned with `possibly_frozen: true` and `frozen_reason` by the order endpoints. The matcher probes it again every 5
minutes; a probe that passes clears the flag. The flag is raised by the co-located matcher (`kob-executor run`), not by a
standalone `index`. Templates whose registry `capabilities` include `freeze`, `seize` or `blacklist` are labelled (`powers`).

**One scale per token: `scale = 10^decimals`, at most `10^9`** (`kob_protocol::defaults::default_scale`; `GET /v1/tokens`
`scale`). An order is an amount of BASE UNITS at a price in sompi per WHOLE token, `scale` base units of its token; orders of
one token are comparable only at one scale, so an order of a token whose entry has `decimals` is listed only at its standard
scale (`non_standard_scale` otherwise). Rules evaluated per order: token allowlisted, template hash, family and extension
match, standard scale (skipped when the entry has no `decimals`), minimum order value (`min_order_value_sompi`: the order's
amount at its quote, `quoteOf(amountLeft, price, scale)` rounded down, a conditional order at its take-profit leg; a bid, whose
quantity is its escrow, the KAS of its output: `order_value_below_minimum` otherwise), expiry at most 90 days (plus slack)
after the accepting chain block, minimum carrier (`min_carrier_sompi`, default 0: the wallet default carrier is 2 KAS,
`DEFAULT_ORDER_CARRIER`, and nothing the matcher or a keeper pays depends on it: their relay fees do not price storage mass,
a refund keeper is paid by the order's `refundTip`). The reason an order is unlisted is stored in `orders.unlisted_reason` and
returned by the API.

### 5. Read API

Everything under `/v1`, JSON, read only. Sompi-denominated fields and token base-unit quantities (`amount`, `amount_left`,
`filled_amount`, `initial_amount`, `min_fill`) are decimal strings (JS-safe); scales, DAA scores and counts are numbers. Prices
and tips are sompi per whole token (`scale` base units of the order's token). Every order, event and fill carries `confirmations`
(node DAA minus event DAA) and `settled` (>= `settle_depth_daa`), so consumers pick a tier:
**tentative** (any accepted event, may still reorg) or **settled** (documented depth). Facilitators
should default to settled. The mempool tier is not served by the indexer.

| Endpoint | Purpose |
|---|---|
| `GET /v1/health` | full status: follower state, cursor, node DAA, lag (`lag_daa`, `lag_seconds`), `alarms` for the 1/6/12/24 h thresholds, reorg and applied-block counters, record-log position (`records_next_n`) |
| `GET /v1/health/ready` | 200 only when following or catching up with lag below the smallest alarm threshold (load balancers) |
| `GET /v1/tokens` | registry tokens plus, with a strict template list loaded, every token that has listed orders: `ticker` (empty without a registry entry), `covenant_id`, `template_hash`, `template_id`, `extension_commitment`, `family`, `decimals`, `scale` (the standard scale `10^decimals`, at most `10^9`; null without an entry), `standing` (`official` or `unverified`: this operator's reading of the registry), `official_declared`, `genesis_verified` (registry flag, `null` when it does not say), `warnings` (`genesis_unverified`), `registry` (`source`, `sha256` of the document loaded), `powers` (program capabilities: `freeze`, `seize`, `blacklist`, `mint-authority`, ...), `open_asks`, `open_bids` |
| `GET /v1/token-utxos?owner=&token=&spent=&limit=&cursor=` | token UTXOs of tracked tokens with proven states, by owner and / or token (section 5.1) |
| `GET /v1/token-events?token=&limit=` | token identities seen in placement records |
| `GET /v1/books/{token}?depth=&aggregate=` | asks ascending, bids descending by **price per base unit** (`price / scale`); aggregated levels (section 5.2) or per order (with `quote`, `auction`, `deadline`, `scale`, `min_fill`, `amount_left`). KAS books only: pair orders are in none, and an unarmed stop entry (if-done entry with `entryStop`) is a stop order, in no book until armed |
| `GET /v1/pairs?token=` | oriented pairs (base A / quote B) with listed live pair orders, optionally those naming one token (section 5.4) |
| `GET /v1/pairs/{base}/{quote}/book?depth=` | the pair book: `direct` (`KobPair`) and `entry` (`KobIfdPair` at its limit) levels and the implied quotes of the KAS route (section 5.4) |
| `GET /v1/pairs/{base}/{quote}/candles?interval=&from=&to=&limit=` | pair candles derived from the KAS series of A and of B (never from pair fills; section 5.4) |
| `GET /v1/pairs/{base}/{quote}/fills?limit=&before=` | pair-order fills (volume, `counterparty`, `price_source: none`) and the 24 h pair volume (section 5.4) |
| `GET /v1/orders/{covenant_id}` | order terms, status, current outpoint and decoded `state`, `auction`, `quote`, `kill_daa`, `refund_due_daa`, `deadline`, `repeat`, `custody`, `strays`, parent/children; pair orders add `pair` (5.4) |
| `GET /v1/orders?maker=&token=&status=&limit=&cursor=` | orders of a maker or token (a pair order is listed under both its tokens), keyset paging |
| `GET /v1/orders/{covenant_id}/events` | lifecycle events: `create`, `fill`, `arm`, `trail`, `rearm`, `cancel`, `refund`, `kill`, `unknown`; `arm`, `trail` and the `fill` of a triggered stop carry `detail.evidence` (`input`, `order`: the evidence order's covenant id, `side` 1 ask / 2 bid, `price`: its quote) |
| `GET /v1/fills?token=&side=&limit=&before=` | recent fills |
| `GET /v1/strays?maker=&limit=` | live stray token UTXOs with the order they are stuck on |
| `GET /v1/trades/{token}?limit=&before=` | trades (the fills of one transaction on one token), newest first (section 5.3) |
| `GET /v1/candles/{token}?interval=&from=&to=&limit=` | OHLCV candles (`interval` 1m, 5m, 1h or 1d), ascending, empty buckets omitted (5.3) |
| `GET /v1/stats/{token}` | 24 h last / open / high / low / change, volume, trade count, best bid / ask, spread, open orders (5.3) |
| `GET /v1/depth/{token}?levels=` | cumulative depth snapshot of the listed book, levels merged by price (5.3) |
| `GET /v1/ws` | WebSocket: subscribe to `health`, `reorg`, `fills`, `fills:<token>`, `book:<token>`, `order:<id>` (a pair order's change is a `book` notice for both its tokens) |

#### 5.0 Protocol v3 field names

Protocol v3 has no lots: every quantity is token base units, every price and tip sompi per whole token (`scale` base units of
the order's token; a pair order's price is token B base units per whole A). The views renamed their fields accordingly (decimal
strings unless noted):

| View | Before (protocol v2.6) | Now |
|---|---|---|
| `GET /v1/tokens` | `lot_size` | removed; `scale` (number: `10^decimals`, at most `10^9`) added |
| book level (`aggregate=true`) | `lots` (number), `lots_estimated`, `lot_units`, `unit` | `amount`, `amount_estimated`, removed, `scale` (number) |
| book order (`aggregate=false`) | `tip_lot`, `lot_units`, `unit`, `remaining_lots` (number), `lots_estimated` | `tip`, `min_fill`, `scale` (number), `amount_left`, `amount_estimated` |
| order (`/v1/orders`, `/v1/orders/{id}`) | `unit`, `lot_units`, `tip_lot`, `lot_max`, `initial_lots` (number), `filled_lots` (number), `remaining_lots` (number), `lots_estimated` | `scale` (number), `min_fill`, `tip`, `budget_rate`, `initial_amount`, `filled_amount`, `amount_left`, `amount_estimated` |
| order `repeat` | `rpt_lots` (number), `rearms_left` (number), `rpt_lot` | `rpt_amount`, `rearm_amount`, `rpt_price` |
| order `cross` | `lot_size`, `b_lot`, `tip_lot`, `net_lot`, `b_lot_end`, `worst_net_lot`, `lots_left` (number) | `scale` (number), `price`, `tip`, removed (`price` is the net rate), `price_end`, `worst_price`, `amount_left`; `min_fill` added |
| order `auction` (`kind: cross`) | prices from `bLot` to `bLotEnd` | from `price` to `priceEnd` |
| order `state` | the contract field names of v2.6 | the contract field names of v3 (`scale`, `minFill`, `amountLeft`, `tip`, `maxFill`, `minTouch`, `rptAmount`, `rptPrice`, `prefund`, `price`, `priceEnd`, ...) |
| event (`/v1/orders/{id}/events`, `/v1/fills`) | `lots` (number) | `amount` |
| event `detail` | `previous.tipLot` (amend), `b_due` = `n × rate`, `tip_kas` = `n × tipLot` (cross fill) | `previous.tip`, `b_due` = `ceil(n × rate / scale)`, `tip_kas` = `floor(n × tip / scale)` |
| WebSocket `fills` notice | `lots` (number) | `amount` (number, base units) |
| trades, candles, stats, depth | `price_basis` = the allowlist lot | `price_basis` = the token's standard scale (sompi per whole token); `decimals` added |
| `unlisted_reason` | `non_standard_lot`, `lot_value_below_minimum` | `non_standard_scale`, `order_value_below_minimum` |
| reject reason | (none) | `retired_template:<kind>` for each record of payload versions 2 and 3 |

#### 5.2 Aggregated book levels

`price` is sompi per whole token, `scale` base units of the order's token. The listed orders of a token with `decimals` all
have its standard scale; a token without `decimals` may have orders of several scales, whose `price` values quote different
whole tokens. A level therefore groups by `(price, scale)`:

```json
{ "price": "250000", "amount": "11000", "amount_estimated": false, "orders": 2, "scale": 1000 }
```

(`price` and `amount`, base units summed in 128 bits, are decimal strings; `orders` and `scale` numbers; `amount_estimated` marks a
level holding a bid, whose amount is its buying power.) Levels are ordered exactly by the price per base unit `price / scale`
(asks ascending, bids descending; ties by `price`, then `scale`), and `depth` cuts on that order. Two levels of one price per
base unit are two rows; a client that wants one row per price merges equal `price / scale` itself. Per-order rows
(`aggregate=false`) carry `scale`, `min_fill`, `tip` and `amount_left`.

#### 5.3 Market data: trades, candles, 24 h statistics, depth

Derived at request time from the fill events and the listed book (`indexer::market`); nothing extra is stored. All prices are **sompi
per `price_basis` token base units** (every response carries `price_basis`: the token's standard scale `10^decimals`, i.e. sompi per
whole token, when the registry knows its decimals, else the scale of the token's first order; and `decimals`, `null` without a
registry entry, so a client can show whole tokens), amounts are token base units, `quote` values sompi, all decimal strings;
timestamps are the chain block's unix milliseconds.

* **Trade** = every fill event of ONE transaction on ONE token. `amount` is the ask side's filled base units (the bid side's when
  no ask filled: a wallet took a resting bid directly, as swap-and-pay does), `quote` the same fills' `quoteOf(amount, price, scale)`
  rounded down per fill (a fill of another scale is converted to the basis: `price × price_basis / scale`). The
  **resting** side is the side whose orders are older (smaller genesis DAA; a tie goes to the asks): its volume-weighted price is the trade
  price, and `side` names the **aggressor** (`buy` when the asks rested). `id` is the smallest fill event id of the trade (paging:
  `before`). Integer arithmetic, rounded down.
* **Candles**: bucket = `floor(ts / interval) * interval` (UTC), `o h l c volume quote_volume trades`; buckets without trades are omitted
  (clients carry the close forward). Without `from` the window is the `limit` intervals (default 500, at most 1500) ending with the
  newest trade's bucket.
* **Stats**: the 24 h window ends at the newest chain block time the indexer recorded (not the wall clock), so a stalled follower
  shows stale numbers instead of an empty day; `change_24h_bps` = (last - first trade of the window) / first. `best_bid` / `best_ask` /
  `spread_bps` come from the listed book; any value that does not exist yet is `null`.
* **Depth**: the listed book's aggregated levels converted to the basis, equal prices merged, best first, with `cum_amount` and
  `cum_quote`; `estimated` marks bid levels whose amounts are buying-power upper bounds.

#### 5.4 Pairs: `GET /v1/pairs`, `/v1/pairs/{base}/{quote}/book`, `/candles`, `/fills`

A pair `base/quote` (two token covenant ids, hex32) is **oriented**: its pair orders are those whose base token A is `base` and
quote token B is `quote` (their prices are B base units per whole A); an order of B/A is in the B/A views, never inverted into
these (`indexer::pairs`). Every book price is `price_num / price_den` **quote base units per base base unit**, a reduced
fraction in decimal strings; amounts are **base** base units (decimal strings). Only listed live orders count: open or partially
filled, exact custodies (one live custody UTXO per custody of the state, at its amount), not flagged possibly frozen, active at
the node's DAA score, not past their refund time (expiry, 90 days idle, IOC / FOK kill) or UTC deadline.

`GET /v1/pairs/{base}/{quote}/book?depth=` (`depth` levels per side, default 20, at most `max_page_size`; `base == quote` is a
400, an unknown pair an empty book):

```json
{ "base": "<hex32>", "quote": "<hex32>", "daa_score": 123,
  "asks": [ { "source": "direct", "price_num": "1", "price_den": "1", "amount": "10000", "orders": 1 },
            { "source": "entry", "price_num": "1", "price_den": "1", "amount": "10000", "orders": 1 } ],
  "bids": [ { "source": "route", "price_num": "2601", "price_den": "1999", "amount": "3074", "orders": 2 } ] }
```

* `asks` are ways to **buy** base with quote, ascending price; `bids` ways to **sell** base for quote, descending. At one price
  the pair orders come first (`direct`, then `entry`, then `route`). `daa_score` is the DAA score the quotes are evaluated at
  (the node's, else the indexer cursor's).
* **direct**: `KobPair` orders. An ASK is an ask, a BID a bid, at its quote now (`price`, or a decaying ask's / rising bid's
  auction price at `daa_score`) divided by `scale(A)`; `amount = amountLeft`. Orders of one exact price form one level.
* **entry**: `KobIfdPair` entries resting at a limit (a limit entry, or a stop entry once armed: its auction price now); a
  sell-first entry is an ask, a buy-first one a bid; `amount = amountLeft`.
* **Left out**: every `KobCondPair` (stops, take-profits, OCO and the exits of entries) and unarmed stop entries, as the KAS
  books leave out their KAS counterparts (nothing fills a stop before its trigger; a take-profit leg is not resting liquidity).
* **route**: route bids (sell base → KAS → quote) walk the base token's plain KAS bids (best first by all-in sompi per base unit,
  `(quote + tip) / scale`) against the quote token's plain KAS asks (cheapest first, all-in `(quote − tip) / scale` per quote
  unit); route asks (buy base with quote) walk the quote token's KAS bids against the base token's KAS asks. Quotes are taken at
  `daa_score`. Each step pairs the current bid and ask and consumes the smaller of their KAS amounts (`floor(amount × all-in /
  scale)`, a bid's amount its buying power); its price is the ratio of the two all-in prices per base unit, its amount the base
  units that KAS buys or sells. Consecutive steps at one price merge (`orders` counts the KAS orders of the level). Indicative:
  they ignore minimum fills, per-fill rounding, fees, token slot limits and the matcher's profit rule.
* **Rounding**: prices are exact reduced fractions whenever both terms fit in 64 bits; otherwise they are rounded to terms that
  do, **down for bids and up for asks**. Route amounts are floored. Integer arithmetic throughout.

`GET /v1/pairs?token=` returns a JSON **array**, one entry per oriented pair with at least one listed live pair order (exact
custodies, active, not due for a refund), optionally only pairs naming `token` as base or quote, sorted by (base, quote):

```json
[ { "base": "<hex32>", "quote": "<hex32>", "direct_asks": 1, "direct_bids": 0, "entry_asks": 0, "entry_bids": 1, "conditionals": 2 } ]
```

(`direct_asks` / `direct_bids`: live `KobPair` ASKs / BIDs; `entry_asks` / `entry_bids`: sell-first / buy-first `KobIfdPair`
entries; `conditionals`: live `KobCondPair` orders.)

`GET /v1/pairs/{base}/{quote}/candles?interval=&from=&to=&limit=` (`interval` 1m, 5m, 1h, 1d; `limit` default 500, at most 1 500):
pair candles **derived from the two KAS series** (the founder price rule: a pair fill never makes a price). For every bucket in
which A or B traded in KAS (5.3 candles, prices per each token's `price_basis`): `o` = open(A) / open(B), `c` = close(A) /
close(B), `h` = high(A) / low(B) and `l` = low(A) / high(B) (the bounds of the implied rate the two series allow in the bucket,
not observed trades). A side without a KAS trade in the bucket carries its last close forward (also from before the window;
`a_traded` / `b_traded` say which side traded); buckets before both tokens have a KAS price are omitted. Without `from`, the
window is the `limit` intervals ending with the bucket of the newest KAS trade of either token. Each rate is

```json
{ "value": "2153", "num": "28000", "den": "13" }
```

**B base units per `price_basis` base units of A** (`pa × basis(B) / pb` for A's KAS price `pa` per `basis(A)` and B's `pb` per
`basis(B)`): `num / den` the exact reduced fraction, `value` its floor (rounded down). The view carries `price_basis` (A's),
`quote_price_basis` (B's), both tokens' `decimals`, `price_source: "kas_books"` and per candle the pair volume of the bucket
(`pair_volume_a`, `pair_volume_b`: base units of A and B of the pair fills, `pair_fills`: their count).

`GET /v1/pairs/{base}/{quote}/fills?limit=&before=` (`limit` default 50, at most `max_page_size`; `before` a fill id): the pair
fills of the pair, newest first, `next_cursor` for the next page, and `volume_24h` (`amount_a`, `amount_b`, `fills` over the
24 h ending at the newest chain block time the indexer knows):

```json
{ "id": 7, "txid": "<hex32>", "ts": 1790000000000, "daa": 123, "order": "<hex32>", "contract": "KobPair", "side": "ask",
  "amount_a": "4000", "amount_b": "4000", "price": "1000", "price_num": "1", "price_den": "1", "a_scale": 1000,
  "tip_kas": "40000", "counterparty": "route", "price_source": "none", "confirmations": 12, "settled": false }
```

A pair order's `GET /v1/orders/{id}` view: `token` is A, `side` 1 (ask) or 2 (bid), `in_book` false, `price` / `quote` null,
`custody` the first custody (`ok` only when every custody is exact), `strays` of both tokens (`foreign` false), and

```json
"pair": { "base": "<A>", "quote": "<B>", "base_family": "kcc20", "quote_family": "kron",
          "base_template_hash": "<hex32>", "quote_template_hash": "<hex32>", "base_scale": 1000, "quote_scale": 1000,
          "side": "ask", "price": "1000", "price_num": "1", "price_den": "1", "stop_price": "1100", "quote_now": "1000",
          "amount_left": "6000", "prefund": "200", "delivery_carrier": "200000000",
          "custodies": [ { "token": "<A>", "role": "base", "expected_amount": "6000", "utxo": { ... }, "ok": true },
                         { "token": "<B>", "role": "quote", "expected_amount": "1209", "utxo": { ... }, "ok": true } ] }
```

(`price` is B base units per whole A: a `KobPair`'s price (a decay's start), a `KobIfdPair`'s limit, a `KobCondPair`'s take-profit
/ limit leg (null without one); `stop_price` a conditional's stop or an entry's `entryStop` (absent otherwise); `quote_now` the
auction price now, else `price`; `prefund` a sell-first entry's B prefund per whole A; `custodies` the custodies of the current
state in record order, `utxo` / `ok` on single-order lookups and the live orders of a `maker=` list; `amount_left` and
`custodies` are empty once closed. The order's `auction` kinds are `pair_decay`, `pair_rise`, `pair_stop`, `pair_entry`, prices in B
per whole A.)

#### 5.1 `GET /v1/token-utxos`: token holdings

Query: `owner` is a 32-byte x-only public key **or** covenant id in hex (an order id lists its custody and strays), or a Kaspa
P2PK address (a script-hash address is a 400); `token` is the token's covenant id (hex32). At least one of the two is required
(400 otherwise). `spent=false` (default) lists live UTXOs, `spent=true` also the spent ones. `limit` (default 100, at most
`max_page_size`) and `cursor` (the previous `next_cursor`) page oldest first. Response `{"items": [...], "next_cursor": null | "<cursor>"}`.

One item (the leading fields are those of the custody / stray token UTXO views, the rest is added):

```json
{ "txid": "<hex32>", "index": 1, "token": "<token covenant id hex32>", "family": "kcc20",
  "program": "KCC20Ref_8x8", "template_hash": "<hex32>",
  "owner": "<hex32>", "owner_kind": 0,
  "amount": "300", "value": "1000000000", "role": "owned",
  "state": { "amount": "300", "owner": "<hex32>", "owner_scheme": 0, "borrow_scheme": 0,
             "borrow_guard": "<hex32>", "extension_commitment": "<hex32>" },
  "state_hex": "<112 bytes hex>",
  "created_daa": 123456, "spent": false, "spent_txid": null, "confirmations": 12, "settled": false }
```

* `family` is `kcc20` or `kron`; `program` is the token program template (`KCC20Ref`, `KCC20Ref_8x8`, `KronToken2433`,
  `KronToken2732`, ...), the argument of kob-wasm's `tokenScriptPublicKey` and of the builders' token requests.
* `owner_kind`: KCC-20 `owner_scheme` (0 key, 4 covenant id); KRON `id_type` (0 key, 1 script hash, 2 covenant id, 3 address
  presence). A key-owned UTXO the builders can spend is KCC-20 kind 0 or KRON kind 3. The query matches the 32-byte owner
  whatever the kind, so filter on `owner_kind` when spending.
* `state` is the token state JSON exactly as `kob_protocol::state::TokenState` serialises it (KCC-20: the fields above; KRON:
  `{"owner","id_type","amount","is_minter"}`), the shape kob-wasm's builders take back in `tokens[].state`; `amount` and
  `value` are decimal strings, `state_hex` is the state span (112 bytes KCC-20, 46 bytes KRON).
* `role`: `owned` (any holder that is not a KOB order), `custody` or `stray` (owned by an order id, as in `custody` /
  `strays` of `GET /v1/orders/{id}`).

**What is indexed, and why it can be trusted.** A token output is a P2SH output: its script hash commits to the whole state, so
the state is not readable from the output. It is read from the transaction that *created* it (the KCC-20 transfer leader's
`next_states`; the columns of a KRON token input, which every input of the token carries) and kept only if the P2SH of that
state under the pinned token program equals the output's script public key (the same proof custody and strays already
use). Only tokens in the allowlist are tracked (with `require_allowlist = false`: every token an order trades). An empty
allowlist tracks nothing. A transaction that creates such an output, or spends a tracked one, is a relevant transaction
and is written to the record log; a rebuild by `index replay` reproduces the holdings. Reorgs revert them like every
other row (created rows deleted, spent rows live again).

**Limits (answers are never guessed):**

* **Genesis outputs are not listed.** The outputs of an issuance transaction have no leader: no transaction reveals their state
  until they are spent, and the spend is the moment they stop being unspent. The same holds for any output whose creating
  transaction did not reveal it (a token program the indexer does not pin, an unrecognised leader). The web app keeps its local
  tracker for the issuer's own genesis outputs; the indexer lists everything from the first transfer on.
* Tokens sent before the indexer started (`--start sink`, or blocks older than its start) are unknown until they move.
* A holding is listed once its transaction is in a chain block the indexer applied (`confirmations` and `settled` say how
  deep); the mempool is not served. Spent-ness is exact only for spends the indexer saw: a holding spent
  inside a gap (section 7.4) stays listed as live (only the custody rows of closed orders are marked spent), and a stale
  row can never be corrected by a rebuild. Consumers must therefore check a UTXO against the node before spending it (the web
  app verifies every candidate on the node); an indexer answer is a lead, not a lock.
* Volume: every transfer of an allowlisted token becomes a record (about 1 KB), so the log grows with the token's traffic,
  not only with KOB's.

Order status: `open`, `partial`, `filled`, `cancelled`, `refunded`, `killed` (IOC / FOK: the
unfilled rest returned, by the same transaction or by a keeper), `closed` (spent by something the
indexer could not classify, or closed by gap reconciliation). `expired: true` marks an open order
past `expiry_daa` whose refund is pending. A repeating entry that sold out stays `partial` with
`amount_left: "0"` until it is refunded (`close`) or cancelled.

#### Hostile input: what one order, payload or request can and cannot do

Every operator runs this code and anybody may run an executor, so a single hostile order, payload or request must not crash,
stall or drain any executor:

* **Numeric gate (`sanity.rs`).** A decoded order state passes the protocol's numeric gate (`AnyState::check_numbers`: `scale` a
  power of ten in `1..=10^9`, every full fill at every rate the order carries worth less than 2^62), the covenant's own acceptance
  rules (`minFill >= 0`, a bid's `minFill > 0`, `tip >= 0`, `slope >= 0`, `decayStep > 0` when decaying, `tif` 0..2, a stop at most
  `MAX_STOP_PRICE`, ...) and bounds on every field (prices to 2^60 sompi per whole token, notional to 2^61 sompi, carriers over
  every possible fill to 2^61, amounts and deliveries to an `i64`) that keep every sum the matcher forms inside its integers.
  Last, the token program(s) the state pins must resolve exactly as the builders resolve them (`kob_protocol::build::order_programs`:
  a supported template hash, its prefix and suffix lengths, the family the order kind trades; a pair order both tokens), so no
  order the planner accepts is one the builder refuses (`bad_state:tokenProgram:not_the_pinned_program`).
  A state that fails is indexed (status, cancel) but unlisted with `unlisted_reason = bad_state:<field>:<why>`, including exits
  (they used to inherit the parent's listing). The gate runs again when the book is read and in the candidate generator, so a
  snapshot file cannot bypass it.
* **The matcher never panics by construction and survives if it does.** Affordable fills are solved in closed form (no
  O(`amountLeft`) loops: a bisection over the covenant's affordability test), quantities and prices are compared exactly in
  128-bit cross-multiplication, the tick has a wall-clock budget (`--tick-budget-ms`, default 20 s;
  one batch's planning gets 5 s) and each batch is planned under `catch_unwind`: on a panic the offending order is found by removing one order
  at a time (those of the plan that panicked, else those of the books whose planning panics alone), quarantined for an hour (logged,
  `StepReport.quarantined`) and the rest of the view is still matched.
  A builder refusal that names the order at fault (`order of leg <i>: ...`: its token programs, its family) quarantines that
  order the same way (`refused by the builder: ...`) and the batch is planned again without it, so one order the builder cannot
  spend never drops the other fills of the batch or stops the other books.
  The keeper tick is guarded the same way and never builds from an order that fails the gate.
* **Planner cost.** The global batch planner sorts the candidates once per plan, stops each walk where nothing further can
  cross it and, once a transaction is full, only extends the legs already in it; the tick budget bounds a hostile book. No candidate
  cap by default (`--max-candidates-per-group` 0; an operator may still set one per book, class and side). A book of 1 000 bids and
  1 000 asks crossing at a loss used to cost 48 s per tick; 10 000 × 10 000 now plans in about 0.13 s (`matcher.md` §9).
* **Probes.** Removed with the receipts in protocol v2.6: the matcher never buys to source a trigger; a stop arms only
  next to a crossing the batch fills anyway (C, "Stops: triggered and armed in the batch").
* **Keepers.** A refund whose fixed `refundTip` is below a cheap lower bound of its fee is not built; one found unprofitable
  is remembered per (order, outpoint); at most `max_attempts` (128) jobs are built per tick.
* **The record log** keeps facts, not the sender's bytes: an undecodable KOB1 payload becomes a five-byte stand-in
  (`payload:undecodable` in `rejects`); a decodable one keeps only its placement records, at most one per output.
* **The read API.** A cancelled request keeps its pool permit until its blocking read ends, and a pooled read
  is interrupted after 20 s. Candles and stats read at most 20 000 fill rows through a `(kind, token, ts)` index (`window_truncated`
  says when the 24 h window was cut). Scanning routes (candles, stats, depth, trades, books, strays, holdings, pairs) cost
  `api.rate_limit.heavy_route_cost` (5) bucket tokens; a client that is already limited does not drain the global bucket;
  `api.rate_limit.ipv6_prefix_bits` (64) can be set to 48 against an attacker rotating addresses through a large prefix, and the
  /64s of one IPv6 site (`api.rate_limit.ipv6_site_prefix_bits`, 48; 0 = off) also share a site bucket
  (`per_site_rps` 100, `per_site_burst` 300), so rotating through the /64s of one allocation cannot drain the global bucket; a
  WebSocket frame that cannot be sent in 10 s ends the session, and a session that sends no application message (a subscription
  change or `{"op":"ping"}`; protocol Ping / Pong frames do not count) for `api.ws_idle_timeout_ms` (60 000) is closed.
  Below the request guard, the transport is bounded too: a request head must arrive within `api.header_timeout_ms` (10 000; also
  the keep-alive idle limit), a response write that makes no progress for `api.write_timeout_ms` (30 000; 0 = off) closes the
  connection, and at most `api.max_connections` (4 096) HTTP connections are open, `api.max_connections_per_ip` (64; per /64,
  trusted proxies exempt; 0 = no cap) of them from one socket peer and `api.max_connections_per_site` (512; 0 = no cap) from
  one IPv6 site (`api.rate_limit.ipv6_site_prefix_bits`, all its /64s together); a connection over a cap is answered `503` and
  closed (an upgraded WebSocket counts against `max_ws_connections` / `max_ws_per_ip` / `max_ws_per_site` (200 per site;
  0 = no cap; over it: 429 `ws_per_site_limit`) instead).
* **The x402 facilitator** attributes requests to the forwarded client behind `trustedProxies` /
  `clientIpHeader`; `/metrics` is never served to a loopback peer when proxies are configured, when the request carries a forwarding
  header, or when `metricsLoopback` is false (use the admin key); a request with a valid merchant key is limited by its merchant's
  bucket, so anonymous junk from a shared address cannot starve merchants; settlements are capped per merchant
  (`maxSettlesPerMerchant`, 8) as well as globally, and `/verify` has its own cap (`maxConcurrentVerifies`, 64). The listener
  closes a connection whose request head does not arrive within `headerTimeoutMs` (10 000; also the keep-alive idle limit) or whose
  response write makes no progress for `writeTimeoutMs` (30 000; 0 = off), and holds at most `maxConnections` (1 024) connections,
  `maxConnectionsPerIp` (64; per `ipv6PrefixBits`, 64; `trustedProxies` exempt; 0 = no cap) of them from one socket peer and
  `maxConnectionsPerSite` (128; 0 = no cap) from one IPv6 site (all /64s of one `ipv6SitePrefixBits` prefix, 48; 0 = no site
  limits); a connection over a cap is answered `503` and closed. A request without a merchant key takes a token of its client
  (`rateLimit.perIp`, burst 30, 10/s), of its IPv6 site (`rateLimit.perSite`, burst 150, 50/s) and of one bucket all such
  requests share (`rateLimit.anonymous`, burst 1 000, 300/s), each only once the narrower ones passed (429 + `Retry-After`).
  Before any chain lookup, a payment's embedded utxos are bounded: a P2SH input must carry the redeem script its script
  hashes, at most 8 distinct other scripts are resolved (each is an address-wide lookup on the node), and a
  `standard-native` payment's authorization is verified against the key of the input it names first, so a payload its
  payer did not authorize resolves nothing. `/verify` refuses a
  payment funded by an immature coinbase output (100 DAA; it is a check, never a delivery guarantee: only `/settle`'s observed
  finality is). `":memory:"` ledgers are refused on mainnet, and a `pending` entry the node never saw is failed and its outpoints
  released after 15 minutes.
* **The hot key**: the hex text read from the key file or `KOB_OPERATOR_KEY` is wiped after parsing. On Windows the file's
  ACL is read (`icacls /save`, SDDL) and a warning names any group that may read it (Everyone, Users, Authenticated Users, ...);
  `KOB_KEY_ACL_STRICT=1` makes that an error. `remove_var` does not scrub the environment block: prefer a key file or a systemd
  credential. `--pause-file` also stops resends of transactions a reorg moved back to pending. A submit that gets no
  answer (transport error, timeout) is not a rejection: the transaction is tracked as pending with its inputs reserved, resent
  every tick until the node answers (accepted / already known: settled; refused: dropped and the orders back off) and dropped at the
  pending timeout (`kob_matcher_unknown_submits_total`).
* **The compute-budget table.** The generator measures every role in the largest batch contexts, with the smallest and the
  largest amounts per pad leg, every multi-cross route the planner can build (C, "Families"), and lowers each role's
  budget by one to prove the engine then rejects it (`kob-protocol/tests/budget_table.rs`); `tests/planner_mixed_scales.rs` fuzzes
  books of mixed scales and amounts on every program and requires that the safety-net retry never fires (`TickReport.slack_retries`). The retry
  stays as a counted safety net.
* **Other executors are not trusted.** Races, replacements and stolen fills are normal: every submitted transaction is engine-validated
  and its profit is read from the built transaction, never from the plan, so a lost race costs nothing.
* **Registry facts, not verdicts.** `/v1/tokens` returns the registry's raw flags (`official_declared`, `genesis_verified`) and the
  registry document the operator loaded (`registry.source`, `registry.sha256`); `standing` and `warnings` are that operator's
  reading of it. `genesis_verified: false` withdraws `official`; `genesis_unverified` is warned for every token whose genesis the
  registry does not confirm (a covenant id only commits to the genesis output hashes, so a hidden genesis output could mint
  look-alike tokens); `ListingRules.require_genesis_verified` (off by default) lists only verified tokens. `possibly_frozen` carries
  `possibly_frozen_basis`: it is this operator's own engine pre-simulation, not chain data.

#### Rate limiting and proxies

Token buckets per client and globally, plus a concurrency cap, request timeout, WebSocket caps
(total, per client, message rate, subscriptions) and a bounded client table. Behind a proxy the
socket address is the proxy, so all users would share one bucket. Set `trusted_proxies` to the
proxy/CDN egress ranges; requests from those peers are keyed by `client_ip_header` (for
`x-forwarded-for` the right-most address that is not itself a trusted proxy, so client-supplied
left-most entries cannot spoof). Requests from any other peer ignore the header. IPv6 clients are
keyed by /64. An invalid `trusted_proxies` entry fails startup.

Put a CDN or reverse proxy (TLS, caching of `GET /v1/books/*` for a second or two) in front, bind the
indexer to loopback, and never point the API at the node.

### 6. Monitoring and alarms

Poll `/v1/health`, or scrape `GET /v1/metrics` (the same numbers as Prometheus text: `kob_indexer_caught_up`,
`kob_indexer_lag_daa`, `kob_indexer_lag_blue`, `kob_indexer_batch_window_blue`, `kob_indexer_bytes_per_tx`,
`kob_indexer_vspc_timeouts_total`, `kob_indexer_within_lag_tolerance`, `kob_indexer_lag_tolerance_daa`, `kob_indexer_prefetch_windows`, `kob_indexer_prefetch_in_flight`, `kob_indexer_prefetch_bytes`,
`kob_indexer_prefetch_bytes_peak`, `kob_indexer_prefetch_discards_total`, ...). Suggested alerts:

| Condition | Meaning |
|---|---|
| `state = gap` | halted, operator action (section 7); page immediately |
| `state = node_unavailable` for more than 5 min | node down, unsynced, or in transitional IBD |
| `lag_seconds` above 1 h / 6 h / 12 h / 24 h | approaching the node's retention; 12 h is consensus finality, 24 h leaves little of the default 30 h pruning window |
| `last_progress_unix_ms` older than 60 s | the follower is stuck (progress is stamped on every successful poll, including "nothing new") |
| `vspc_timeouts_total` rising, `batch_window_blue` small, `lag_blue` not falling | the link to the node is slower than the chain (section 3, *Bandwidth*): run the indexer next to its node |
| `prefetch_discards_total` rising steadily | the chain keeps reorganising under the windows fetched ahead (a node far behind its own peers, or a deep reorg); each discard costs one window of traffic, the state stays exact |
| `prefetch_bytes_peak` at `prefetch_max_mb` and `lag_blue` falling slowly | the budget, not the link, limits the catch-up: raise `prefetch_max_mb` if memory allows |
| `consecutive_failures` above 10 | every poll fails (node, link or database); `last_error` says which |
| `reorgs_total` jumps or `reverted_blocks_total` > 100 in an hour | network instability |
| `records_next_n` not increasing while `relevant_txs_total` grows | record-log write failure |
| `node_version` behind the newest release | upgrade drift |
| disk above 70 % (data dir and node) | |

Cross-check against a second node/instance: compare `cursor_hash` and the order count, or the
`export-orders` output hash, between the two sites once per minute.

### 7. Runbook

#### 7.1 Normal restart or short downtime

Nothing to do: stop, start. The follower resumes from the stored cursor. The API keeps serving the
last committed state while it catches up (`/v1/health` shows `catching_up` and the lag).
Downtime shorter than the node's retention (default 30 h, 7 days with `--retention-period-days=7`)
loses nothing. Reorgs during the downtime are handled by the node's `removed` list.

#### 7.2 Crash safety

The record-log frame is appended and fsynced before the database transaction commits. After a crash
the log can hold one frame the database never committed; `open` cuts it off and reports it
(`repaired the record log`). A torn last frame is removed the same way. If the database is *ahead*
of the log the log was lost; the indexer refuses to start rather than silently continue without its
rebuild source. An empty database next to a populated log is refused as well (use `replay`).

#### 7.3 The node forgot the cursor (reset, resync, or a reorged-out block that was pruned)

`cannot find header`: the follower walks the stored chain blocks (the reorg window) back to the
newest one the node still knows, reverts everything above it, and continues (logged as `Rewound`). If
the node knows none of them, the state becomes `gap`.

#### 7.4 Gap: downtime longer than the node's retention (`state = gap`)

`the queried hash does not have retention root on its chain`, or no stored block known. The follower
stops and never guesses. The chain between the cursor and the node's retention root is gone from the
node; everything the indexer recorded before the cursor is safe in the record log, but orders created
inside the gap cannot be rediscovered from the node (their placement records are gone). Recovery, in
order of preference:

1. **Restore a newer backup** (database + record log from the standby or off-site copy). If the
   backup's cursor is still inside the node's retention the follower continues seamlessly.
2. **Second site.** Copy the standby's database (`VACUUM INTO`, section 8) and record log over, start.
3. **Rebase and import** (data loss limited to what nobody can supply):
   ```
   kob-executor index rebase --to sink          # moves the cursor to the node's sink and
                                                # reconciles tracked orders with the node's UTXO set:
                                                # spent outputs are closed with reason "gap", outputs
                                                # that continued under the same script are adopted
                                                # at the node's UTXO DAA, with their custody looked
                                                # up at the script the state derives and the strays
                                                # the node still holds; the rest of their token rows close
   kob-executor index import-orders orders.json # recovery hook, see below
   kob-executor index run
   ```
   Orders that were created *and* still open at the gap, and are not in any export, stay invisible
   to the book until their maker imports them (7.5) or cancels them from the placement records: the
   web wallet's Recover panel, or `kob recover --cancel` from its backup or an export (7.5). The
   maker's own placement record is the fallback that makes cancel work even if KOB disappears. Fills and
   partial fills inside the gap are not recorded as events: an order that continued under a NEW
   script inside the gap (an ask partially filled, an armed stop, ...) is first closed (status
   `closed`, reason `gap`); importing its current state (7.5) revives it (event `update`, reason
   `import_revive`: the node-verified output and custody become live again).

#### 7.5 Maker-side recovery hook (`export-orders` / `import-orders`)

The export format is the maker's *order receipt*: one entry per order with the template hash, the
current state span, the extension commitment of the custody token (ask side) and the covenant id. The
UI/CLI that places an order stores it (it is the placement record); a healthy indexer can produce it
(`kob-executor index export-orders --maker <pubkey> --live-only`), and archive dumps (api.kaspa.org,
kascov) can be converted to it.

```json
{ "version": 2, "network": "testnet-10",
  "orders": [ { "template_hash": "<64 hex>", "state": "<state span hex>",
                "extension_commitment": "<64 hex>", "covenant_id": "<64 hex>" } ] }
```

Only availability is trusted. For every entry the indexer rebuilds the redeem script from the pinned
template and the state, derives the P2SH address, asks the node (`getUtxosByAddresses`, needs
`--utxoindex`) for unspent outputs at that script that carry a covenant id, and imports exactly those.
For an ask-side entry with an extension commitment it also rebuilds the custody script (`amountLeft`
base units owned by the id) and verifies the custody UTXO the same way; without it the order is imported
with **unverified custody** (reported in `custody_unverified`, and not listed in the books). A forged,
wrong or already spent entry finds no output and is reported as `not_found`. Imports are idempotent,
are written to the record log (a replay reproduces them), and are stamped block 0 (never reverted).
The state span must be the *current* script: after a partial fill that changed the script (IFD
entries) export a fresh state. Version 1 exports (no commitment) are still accepted.

**One writer per data directory.** `import-orders` (like `rebase`, `replay`, `index` and `run`) writes the database and
the record log, so it refuses a data directory another process is writing: stop `run` / `index` first, import, start it
again. Every writer holds an exclusive OS lock on `<data_dir>/kob-writer.lock` and `<records_dir>/kob-writer.lock` for as
long as it runs (released by the OS when it exits, also after a crash; `kob-writer.owner` names the holder for the error
message). `export-orders` and the read API open the database read-only and work next to a running writer. On the TN10
soak (10-01) an import run next to `run` interleaved two writers and broke the record log's hash chain (`record log frame
N is corrupt: chain hash mismatch`); the lock makes that impossible.

**Without an indexer: `kob recover`.** The maker finds and cancels their orders from a node alone:

```
kob recover --from backup.json --from orders.json --node ws://127.0.0.1:18210 [--maker <pubkey>] [--out report.json]
kob recover --from orders.json --node ws://127.0.0.1:18210 --amount-left <covenant id>=6.5   # the amount a partly filled order has left
kob recover --from backup.json --node ws://127.0.0.1:18210 --cancel --key file:maker.key [--dry-run] [--out-dir signed/]
```

Inputs: the web wallet's backup (`kob-backup`), this export format (version 1 or 2), or indexer order views
(`GET /v1/orders/{id}`, or a page of them). Orders are deduplicated by covenant id; every state a file names is a search
candidate and nothing else in a file is trusted. Each order's template is the pinned one, else a retired template
(an older contract version: spend-only, `docs/spec/template-retirement.md`; its states are read in their older lot
layout and the report marks them `older_contract_version: true`). The candidates are, in this order: the last proven
state and the given state; each with `amountLeft` = the `--amount-left` hints; each with `amountLeft` = original −
k × `minFill` for k = 1, 2, … (at most 200 candidates in all). A partial fill splices the new `amountLeft` into the
script and any amount of at least `minFill` may have been filled, so **this search is not exhaustive**: a partly
filled order the grid does not reach is reported `not_found` until the maker passes its amount with
`--amount-left` (the `amount_left` an indexer view shows; base units, or a decimal amount of whole tokens with at
most the token's decimals: `6.5` with 3 decimals is 6500; `<covenant id>=<amount>` names one order, a bare amount
applies to every order of the run). An order of a retired lot template is searched over every smaller `lotsLeft`,
which is exhaustive for its asks, bids and pair orders. An order is `live` when the node holds an unspent output at a
candidate's exact P2SH script carrying the order's covenant id; ask-side custody (`amountLeft` base units; a pair order: each
custody of its state under its token's program, a sell-first entry's B prefund reported as `prefund` with the entry's `bExt`) is rebuilt
from the live state and verified the same way (`verified`, `missing`, or `unknown` without the extension commitment
of a KCC-20 token). `not_found` is final only for a bid (its script never changes); for the others it can be a state
the search did not reach (an amount off the grid, a moved trailing stop, an armed band, repeat fields): pass
`--amount-left`, or use an indexer view or the web wallet's resolver. `unsupported`: the template is neither pinned
nor retired. The report is JSON (one entry per order: status, outpoint, value, UTXO DAA, current state, `amount_left`
(base units; a bid: its remaining buying power), `scale`, `min_fill`, `found_by`, custody).

`--cancel` builds the maker's cancel of every live order of the key's maker (custody verified when it has one), one
transaction per order: custody and KAS back to the maker, fee from the order's released KAS, else the smallest sufficient
plain KAS UTXO of the maker on the node. Every transaction is signed with the key (a key that is not the maker is
refused), validated in the script engine before anything is written or submitted, written to `<covenant id>.cancel.json`
(signed transaction and `submitTransaction` request), and submitted unless `--dry-run`. Strays are not swept: the node
alone cannot tell them apart. With an indexer view, `kob order cancel --view <file>` cancels with the custody (a pair order: every
custody of its state, the second one, a sell-first entry's B prefund, from `pair.custodies[1].utxo`; a view without it is refused), the strays
of the order's tokens (a pair order: of A and of B; within each program's token inputs) and foreign strays, and reports the strays left behind;
`kob order sweep --view <file>` returns the strays without ending the order (the maker's sweep in place: same covenant id,
same script, custody untouched; the 90-day idle window restarts). Both look up the view's order UTXO, custody and strays on
the node first and refuse a view whose order UTXO the node does not hold (stale view).

#### 7.6 Database lost or outdated, record log intact (`replay`)

```
mv index.sqlite3 index.sqlite3.bad
kob-executor index replay --data-dir /var/lib/kob-index     # builds a NEW database from the log
kob-executor index run ...                                  # resumes from the logged cursor
```

Replay re-applies every logged batch (including reorgs and imports) through the same code as the
live path and re-evaluates listing with the current allowlist and rules. The follower then resumes
from the logged cursor and **re-syncs the tail from the node** (at most the time since the last
logged batch: an otherwise empty batch is logged every `checkpoint_secs`, one hour by default, so
the tail is short even when nothing KOB happened). The same procedure rebuilds the database after
an upgrade that changes the schema or the derivation rules. The log's hash chain is verified; a
broken chain aborts the replay with the frame's number.

The log outlives the templates it recorded. Frames are versioned: since 2026-10-01 (record-log format 2) a frame carries a
template table (record-log code -> template hash of every order template and token program it refers to) and the length of
every revealed state, so it decodes whatever the templates are when it is read. Frames written before (format 1, no
marker) stored a state at its template's length of the time; they are read with today's layouts plus every retired
layout this repository ever committed (`indexer/layouts.rs`, chosen by the entry's dispatch tag, the whole frame
deciding when two fit). A reveal of a template this build does not have (an older artifact of a kind, or a kind it does
not know) is dropped, and a frame it cannot decode at all (a newer format) is kept in the log and skipped; both are
counted in the replay's summary and logged as warnings, never fatal. An order whose template this build does not have
cannot be interpreted either: its placement becomes a reject (`placement:...`) in the rebuilt database. Example, the
TN10 soak's pre-958d013 log (8,388 format-1 frames): all replay, 864 reveals of the old `KobCross` (333-byte state) are
dropped, the 777 old cross limits are rejects, every other order, event and holding matches the database the old build
kept. Format 2 frames are appended after format 1 in the same segment; an older build opens such a log (it checks the
chain only) but cannot replay it.

**Schema 3 (protocol v2.6).** Opening a schema-2 database drops its `receipts` table and indexes in place and records
schema 3; nothing else referred to them, so no replay is needed. An old configuration's `[receipts]` table is ignored.

**Schema 4 (in-place amends, payload version 3).** Opening a schema-3 database adds the `order_amends` table (the terms
an in-place amend replaced, per block, so a reorg restores them) and records schema 4; no replay is needed (a database
written before had no amends). An older build refuses a schema-4 database. Roll indexers out before wallets: a build
before payload version 3 rejects every version-3 payload (`payload:unsupported KOB1 version 3`), so it would list none of
the orders those wallets place and would show an in-place amended order as open with an unknown state. The record log
keeps version-2 payloads of older frames as they were; they replay unchanged.

#### 7.7 Wrong network / DB

The database records its network; opening it with another `--network` is refused. A node reporting
another network id halts the follower (`gap`).

#### 7.8 Drill (run on TN10 before mainnet)

Kill the indexer for 48 h with `--retention-period-days=7` and recover (7.1). Then repeat with a
node retention shorter than the outage to force 7.4 and recover through the standby, and through
rebase plus import. Delete the database, replay from the log, and compare `export-orders` output
with the live instance (7.6). Restore a backup on a clean machine and compare against the live
instance.

### 8. Backups

* Record log: `records/seg-*.kobrec` is append-only; all but the newest segment are immutable, so
  incremental off-site sync (rsync, restic) is safe. Encrypt off-site copies. Verify a copy by
  running `replay` on a scratch machine and comparing `export-orders` output. **This is the asset
  that cannot be recovered from the node after pruning.**
* Database: `sqlite3 /var/lib/kob-index/index.sqlite3 "VACUUM INTO '/backup/index-$(date +%F).sqlite3'"`
  is consistent while the indexer runs (WAL). It is a convenience (it saves a replay); it is
  rebuildable from the log.
* The node database is rebuildable and needs no backup.

### 9. Record log format

`records/seg-<first frame>.kobrec`, a sequence of frames:

```
u32 LE body length | body | 32-byte chain hash = blake3(previous chain hash || body)
```

The chain detects truncation in the middle and edits; segments rotate at `records.segment_bytes`.
The body is one committed batch: the wall-clock time, the `start` hash of the request, the removed
chain hashes (tip-first), the new cursor, the chain blocks that carried relevant transactions (hash,
DAA score, blue score, timestamp), and the operator operations (`import`, `close_gap`, `adopt`). Each operation is
self-describing by its code: since 2026-10-02 an import (code 5) also carries the exit's entry (`parent`), and an adoption
(code 4) the node's DAA score of the adopted output, its custody and the token UTXOs the node still holds; the older codes 1
and 3 are still read (an old adoption then takes the cursor's DAA and closes every token row, as it did). A build from
before cannot read the new codes: replay such a log with this build or newer.
Integers are LEB128 varints, hashes are 32 raw bytes.

A relevant transaction is stored as a *reduced transaction*:

| Field | Content |
|---|---|
| `txid`, position | 32 bytes, position in the chain block |
| `payload` | the KOB1 payload (placement records), only for transactions that carry one |
| inputs | outpoint; for covenant inputs the covenant id and the DAA score of the block that created the spent output; for spent KOB orders: the template (one byte: the KOB1 kind code, with the high bit set for the KRON kinds; logs written before the KRON family only hold codes below `0x80` and read back unchanged; a v2.4 log's receipt reveals, codes `0x07` / `0x87`, are read past and dropped), the revealed **state span**, the 4-byte dispatch tag and the small arguments (arguments over 40 bytes, the template prefix / suffix pushes and signatures, are elided) |
| outputs | value; for covenant-bound outputs the script public key and the binding (authorising input, covenant id) |
| token outputs | output index, amount and owner order id of every token output owned (KCC-20 scheme `0x04`, KRON `id_type` 2) by a tracked or freshly created order id |
| holdings | output index, token program (one byte) and the state span of every output of a tracked token whose state the transfer leader revealed and whose script public key proves it (`GET /v1/token-utxos`). Present only when there are holdings: the top bit of the encoded position flags the section, so logs written before it existed decode unchanged |

A transaction is relevant if it carries a placement record (or a malformed KOB1 payload, recorded as a five-byte stand-in for
the `rejects` table; only placement records of a decodable payload are kept, at most one per output), spends a tracked order or token UTXO, creates
a token output owned by a tracked order, or creates a proven output of a tracked (allowlisted) token, or spends a tracked holding. Only chain blocks that carried relevant transactions, reorgs that
reverted a KOB row (a reorg of unrelated blocks changes nothing and is not logged: TN10 reorgs several
times an hour) and operator operations are logged, plus a checkpoint every `checkpoint_secs`. The log holds
no signature scripts, token programs, template code or unrelated transactions [F, tested].

### 10. Testing

```
cargo test -p kob-executor                      # unit + flow + recovery + e2e (no network)
KOB_SKIP_NETWORK_TESTS=1 cargo test -p kob-executor
cargo test -p kob-executor --test tn10_follow -- --nocapture   # live TN10 node, read only
cargo test -p kob-executor --test storage -- --nocapture       # prints the storage numbers above
cargo test -p kob-executor --test capacity -- --nocapture      # processing smoke (20 blocks of 300 tx), prints the numbers
cargo test --release -p kob-executor --test capacity -- --ignored --nocapture full_capacity   # 3,000 tx/s benchmark (section 3)
KOB_TN10_WRPC=ws://... cargo test --release -p kob-executor --test tn10_capacity -- --ignored --nocapture  # live catch-up by fetch_parallel
```

The flow tests build real v2.6 transactions with the `kob-protocol` builders (real artifacts, real
KCC-20 programs, consensus covenant ids, validated by the rusty-kaspa script engine) and feed them
through an in-memory node: `flow_kinds` (ask, bid, market / IOC / FOK kills, Dutch and rising
auctions, day orders), `flow_cond` (arming next to a resting fill, triggered fills, the evidence rules, trailing, stop-band fills, OCO, stop
entries), `flow_ifd` (IFD / IFO both sides, exits, stop entries triggered next to their evidence, repeat cycles with
merges, the empty repeating entry, cancel all), `flow_strays` (strays, cancel-replace, forged and missing placement records), `flow_amend`
(in-place amends: the order keeps its id and custody and takes the new terms, a reorg restores the old ones, a forged, missing or
two-output continuation of a cancel is listed nowhere), `flow_follower`
(paging, errors, unknown cursors, gaps, the reorg window and the property that the incrementally
maintained database equals a from-scratch build and a rebuild from the record log under random
traffic (in-place amends included) and random reorgs; parallel fetch: windows applied in chain order, a reorg
discarding the windows past the fork with no orphan applied, the memory budget, split on timeout, the same property with
four windows in flight over a slow link), `multinode` (several nodes on one chain, one slow and one behind: the database
equals a clean replay; liars that alter an output, drop a transaction, alter a header or strip the spent outputs of a KOB
transaction are dropped and nothing they altered is applied), `recovery` (restart, replay, crash repair, export / import, reconcile),
`e2e_api` (REST + WebSocket on a socket). `executor_e2e` feeds the same chain data to the
matcher and the keepers through the store (Part A, A.5).

`tn10_follow` uses `KOB_TN10_WRPC` (default `ws://127.0.0.1:18210`: a testnet-10 node of your own), skips itself when
`KOB_SKIP_NETWORK_TESTS` is set or the node is unreachable (`KOB_REQUIRE_NETWORK_TESTS=1` turns an
unreachable node into a failure for nightly CI), and waits for `KOB_TN10_FOLLOW_BLOCKS` (default 3)
new chain blocks after catching up. `tn10_multinode` checks a `Full` window of a public TN10 node (`KOB_TN10_WRPC_OTHER`,
default `wss://boson-10.kaspa.red/kaspa/testnet-10/wrpc/json`) against the primary (`KOB_TN10_WRPC`): real header hashes and
transaction ids (v0 and v1) verify, a tampered copy does not.

### 11. Known limits

* Conditional (trigger) orders are indexed and queryable but not in the price books (`in_book` is for
  resting limits and if-done entries; an if-done stop entry only once armed: unarmed, nothing fills it at its limit before its
  trigger evidence, and its limit, below the market for a sell stop, read as a crossed book); use `/v1/orders`. A database
  written by an earlier build is corrected at start-up (`backfill_stop_entry_books`).
* No mempool tier. The kill switch is the executor's `--pause-file` (Part C); it stops the matcher
  and the keepers, not the indexer.
* A bid's remaining amount is an upper bound derived from its escrow (its buying power; its quantity is a budget).
* After a gap adoption the order's custody is unknown until its next spend, so the order is not
  listed until then.
* `GET /v1/token-utxos` lists holdings whose state a transaction revealed; genesis outputs of an issuance are not listed (5.1).
* KRON tokens held by `id_type` 0 (pubkey) or 1 (script hash) are outside KOB (the builders refuse them); only
  custody (`id_type` 2) and address-presence (`id_type` 3) tokens are handled.
* Covenant ids of if-done exits are taken from the node (consensus-derived); placement-record
  genesis ids are recomputed.
* A `removed` list with no `added` blocks (only possible with `minConfirmationCount`) is retried
  rather than applied (under a batch window: the window widens first).
* Every accepted transaction of the chain crosses the link (about 1.35 KB each at `High`); the node offers no filter
  (section 3, *Bandwidth*). An indexer on a link slower than the chain cannot catch up, however its batches are cut.
* The write path is single-threaded: about 0.5 ms per KOB transaction (placement verification, state derivation, rows),
  so a chain carrying only KOB transactions is followed up to about 1,800 tx/s on one core of the reference PC (section 3,
  *Processing capacity*); realistic mixes are far below that.

---

## Part C. The matcher and the keepers

`kob-executor match` batch-matches crossing KOB orders and arms, ratchets and triggers stops inside its batches;
`kob-executor keep` refunds, kills and closes them; `kob-executor run` does both in the indexer's process (Part A). All of it is
permissionless: anyone may run it, it never holds user funds, and a matcher that misbehaves only
produces invalid or unprofitable transactions (the covenants enforce every price, quantity and
trigger rule).

### What the process does

Every tick (default 1 s) it reads the book and plans **one global batch over every book** (any number of tokens, programs and
both families in one transaction, the pair orders among them, netted and routed), builds it with `kob-protocol`, signs its own inputs, validates
the signed transaction in the rusty-kaspa v2.1.0 script engine and submits it over the node's JSON wRPC; when the crossing book
set is larger than one transaction it plans the next one on the book the previous one leaves, most profitable first, until
nothing profitable crosses or the tick budget runs out (`matcher.md` §3, §7).

| Step | Rule (matcher.md) |
|---|---|
| Quote every order at `t = DAA − 5` | §2.1 (decaying asks, rising bids, stop auctions, stop entries) |
| Drop expired, killed, 90-day-idle orders and day orders past their UTC `deadline` | §2.3 |
| Class 1 (IOC, FOK, market, streaming) first, then armed stops, then resting crossings, then stops triggered by the batch's own plain fills | §3.1, §4 |
| Price → tip → age within a class | §3.1 |
| FOK all-or-none, IOC maximal, per token: its program's slots (3/3, 4/5, 8/8, 16/16; KRON 4/5) and `MAX_TOK_IN`; the physical limits of the transaction (no size or candidate cap by default) | §3.2 |
| Books share a transaction by profit per byte; a larger book set is split into chained transactions | §3.1, §7 |
| Drop fills whose marginal profit is negative (class 1 only if the batch would lose) | §3.2 |
| Stops trigger only next to their evidence, a plain resting `KobAsk` / `KobBid` filled in the same transaction (same book, trigger side, at or through the stop, `n ≥ minTouch`, exposed `minRestDaa` before the lock time): a triggered stop walks after the other classes, at its trigger price; the other stops the evidence serves are armed (trailing stops ratcheted, the most steps) with an `update`, every one while the batch stays profitable (the update's cost is charged, the `keeperTip`, possibly 0, is income; none is dropped unless the batch would fall below `--min-profit`) | §3.2, §4 |
| A drop is kept only if the whole batch's profit rises: a thin crossing that is the evidence of profitable triggered fills or updates stays | §3.2, §4 |
| Booked repeat exits take profit only with their entry's merge; stop-losses never carry the entry; one merge per entry per transaction | §6.1 |
| Chain the next transactions of the tick onto unaccepted continuations (only DAA-independent paths, at most 4 per order) | §3.4, §7 |
| Never spend a stray, never spend an order twice | §1.2, §8 |

The keeper refunds at the order's refund time (soft expiry, 90 days idle), kills IOC / FOK orders
at `max(UTXO DAA, activeFrom) + 600` with that exact lock time (the first block that allows it),
and closes empty repeating sell-first entries. It never arms or ratchets: since protocol v2.6 an `update` needs its
evidence filled in the same transaction, so arming is the matcher's, inside its batches (below). Day orders
need no special handling: matchers stop at the `deadline`, the refund opens at `expiryDaa`.
Pair orders are refunded and killed the same way (their custodies back to the maker for the `refundTip`: a sell-first
`KobIfdPair` returns its A custody and its B prefund, each at its own index); a refund never moves strays of either token
(only the maker's cancel does).

### Stops: triggered and armed in the batch

A stop (stop leg of a `KobCondAsk` / `KobCondBid`, stop entry of a `KobIfdBid` / `KobIfdAsk`, either family) reads its trigger
from a plain resting `KobAsk` / `KobBid` of its token and scale filled in the **same transaction** (`matcher.md`
§4): a sell stop from a resting ask filled at or below it, a buy stop from a resting bid filled at or above it; the evidence
must not decay (`slope` 0), must fill `n ≥ minTouch` base units and must have been exposed at its quote at least
`minRestDaa` before the lock time (`max(UTXO DAA + interval, custody DAA, activeFrom)`). Pair, conditional and
if-done fills are never evidence of the KAS kinds (pair stops read their own evidence, below); an unaccepted continuation (a chained step) is never evidence or updated. Nothing persists:
a stop is only armed by a batch that carries a qualifying fill.

| In a batch | What the matcher does |
|---|---|
| Evidence | the fills of plain resting orders the class walks produced (the planner does not buy to create evidence) |
| Triggered fill | an unarmed stop served by the evidence walks what the other classes left, at its trigger price, in class-2 order among triggered stops; it is never passive liquidity and never takes its own evidence's liquidity, so it is sequenced after the trade that triggers it; lowered with `evidence` = that fill's leg |
| Update | every other listed unarmed stop the evidence serves (and every trailing stop whose UTXO is `trailWait` old and whose opposite-side evidence justifies a step) gets a `BatchUpdate` inside the batch's objective: its cost (its bytes at the fee rate, at least the measured `updateFee` of its token program scaled by `fee rate / 100`) is charged, its `keeperTip` is income, and it is included whenever the batch stays profitable (a tip of 0 arms when the spread pays for it), highest tip first, within the byte budget; only while the batch would otherwise fall below `--min-profit` are updates dropped, the worst `keeperTip` minus cost first (the later in tip order on a tie); a standalone arming batch (its fills pay no spread) keeps an update only when `keeperTip` is at least its cost; one input per covenant id, `activeFrom` and `trailWait` (CSV) respected |
| Income | the batch's change takes the `keeperTip` of each update (a priority fee on top of the crossing spread); triggered fills pay like any fill |
| Next tick | an armed stop runs its band auction with class-2 priority, no evidence needed |

`--no-arm` turns updates off (triggered fills stay). The indexer's events name the evidence (`detail.evidence`, Part B).

### Pair orders

A pair order of tokens A / B (`KobPair`, `KobCondPair`, `KobIfdPair`; one template each for both sides and both families,
`order-types.md`) quotes token B per whole A and enforces only its own guarantees (founder option 2): an ask receives at
least `ceil(n × p / scale(A))` of B for n base units of A, a bid pays exactly `floor(n × p / scale(A))` of B and receives
exactly n of A, both release `floor(n × tip / scale(A))` sompi of their prefunded KAS tip to the matcher. The reference
matcher fills them in the global batch (`matcher::pair`, `matcher.md` §3.5) in two ways, and never from inventory (it sells
no tokens of its own; with the opt-in surplus-inventory policy, below, it may keep a netting surplus):

| Way | |
|---|---|
| Netting (first, in every allocation) | the pair orders selling token X for token Y against those selling Y for X (an ASK and a BID of one pair, or an ASK of A/B and an ASK of B/A), best limits first, any number per side; each at its exact covenant amounts; the token balances of the group kept exact: the B the netted bids pay beyond what the netted asks receive (the surplus) goes to the pair asks' deliveries (minimums: the builder hands it to the first one), or, when that pays, is sold to plain KAS bids of B (the matcher earns their KAS); a sell-first entry's exit custody and a bid's A are exact, so a surplus of a token needs a pair ask buying it |
| Route (the remainder) | a pair order enters the KAS books as two legs sharing its covenant input: its Sell leg sells the token it releases into plain `KobBid`s of that token, its Buy leg buys the token it receives from plain `KobAsk`s of it (a pair ask's purchase may take more when an ask's minimum fill forces it, the excess on its delivery; a bid's purchase is exact) |

| Rule | |
|---|---|
| Eligible | listed, exact custodies (a sell-first entry: its A and its B prefund), UTXO accepted and at most `t` old, active at `t`, not past its soft expiry, IOC / FOK kill, 90 days idle or UTC deadline, not spent by a pending transaction or an earlier transaction of the tick; TWAP / DCA slices `interval` old (CSV) |
| Class | GTC: resting (class 3); IOC / FOK: immediate (class 1); armed pair stops and stop entries: class 2 |
| Priority | netting: the lowest ratio of what an order receives to what it releases first, then class, tip, age; route: each leg at its implied quote (the Sell leg at what the T one whole A of the order needs costs at the best plain ask of T, less its KAS tip; the Buy leg at what the S of one whole A fetches at the best plain bid of S), ranked with the direct orders of those books by price per base unit → tip → age |
| Quantity | FOK all or nothing; IOC the largest fill; every fill at least `minFill` unless it takes everything left (a netted part on its own too); a GTC rest keeps something in the custody and funds its carrier and tip; a route bound caps what each order routes at the largest fill its route pays on its own (on top of its netted part) |
| Transaction | per token: its program's slots (a 2 x 2 netting needs 4 outputs of one token: an 8 x 8 or 16 x 16 program), one extension commitment; no token output to the operator, except a surplus the surplus-inventory policy keeps (below) |
| Profit | the batch as a whole (`Σ bid all-in − Σ ask all-in + Σ pair tips + Σ update tips − fee`, every amount the exact rounded covenant value): a netting moves no KAS (it pays its fee from the pair orders' tips and the surplus it sells), a route its KAS spread; a unit whose margin does not pay its bytes is dropped with both its legs |
| Pair stops | an unarmed `KobCondPair` stop leg or `KobIfdPair` stop entry is armed (an `update`, or filled and routed when it crosses) by evidence of the same transaction in one of two modes: two KAS-book fills (a plain resting order of A and one of B, the implied rate `a × scale(B) / b`, a sell stop reading an ask of A and a bid of B) or a resting `KobPair` of the pair filled in it (a pair ASK for a sell stop); trailing pair stops are ratcheted by the opposite side's evidence (the most steps) |
| Repeat | a booked pair exit's take-profit before `rptUntil` re-arms its `KobIfdPair` entry in the same transaction (the merge); an exit is never updated next to its entry |

The two legs of a routed order are reconciled to one fill `n` (caps only decrease) before the batch is built and validated in
the engine like any batch. A pair order is never chained within a tick. Prices: a pair fill sets no KAS price (the indexer
records trades, candles and last prices only from KAS-book fills; a routed pair fill's KAS-book counterparties produce their
own trades).

The compute budgets of every pair shape (routes of both sides, netting 1 x 1 and 2 x 2, conditional fills and updates in both
evidence modes, if-done fills, re-arms) are in the kob-protocol table.

### Surplus inventory (opt-in)

A crossed pair match (a pair bid above a pair ask) pays its crossing in token B, not KAS. Without tips the matcher earns KAS
only when plain KAS bids of B can buy that surplus in the same transaction, and a wallet bid's minimum fill (10 KAS) is often
larger than the surplus of a small crossing (a 20 USD order crossed by 1 % leaves 0.2 TUSD, about 4.8 KAS). The surplus then
goes to the pair ask's delivery, the batch earns nothing, and the crossed book is not matched. The owner's decision
(2026-10-06): an operator may take such a surplus into its own key and count it as income (`matcher.md` §3.5). The pair ask
then receives exactly its floor `ceil(n × p / scale(A))`, all its covenant guarantees (`KobPair.sil`: `tOut >= ceil`).
It is off by default. `--inventory-policy <file>` (on `run` and `match`) reads it, strict JSON:

```json
{
  "acceptSurplusTokens": true,
  "haircutBps": 8000,
  "keepCarrier": "200000000",
  "tokens": [
    { "token": "<covenant id, hex>", "minAmount": 100000000 },
    { "token": "<covenant id, hex>", "refPrice": { "sompi": "2300000000", "per": "100000000" } }
  ]
}
```

| Key | Default | Meaning |
|---|---|---|
| `acceptSurplusTokens` | `false` | master switch of the planner side; a listed token is never sold by maintenance either way |
| `haircutBps` | `8000` | the share of the valuation counted as income (at most 10000) |
| `keepCarrier` | `200000000` (2 KAS) | sompi locked on the operator's inventory output until the owner sells it (and on the UTXO a maintenance merge of inventory leaves); at least `50000000` (0.5 KAS, the KaspaCom KCC20 0.2.5 floor). The order outputs keep their own carriers. 2 KAS is the smallest round value that adds no fee in either fee mode (below) |
| `tokens[].token` | (required) | an allowlisted token (covenant id); every other token's surplus goes to the pair ask as before |
| `tokens[].refPrice` | none | `sompi` per `per` base units: the owner's own valuation (it sells the inventory off-matcher), used instead of the KAS bids |
| `tokens[].minAmount` | none | the least surplus worth keeping (base units). With `refPrice`: no bound unless set. Without it the bound is the dust rule, the smallest minimum fill of the bids that value the surplus (a sale of the kept amount alone could fill one), and `minAmount` only raises it (a value below the dust rule, `0` included, changes nothing) |

What the planner does (`matcher::batch`, pure, deterministic):

* A surplus that plain KAS bids take in the same transaction is still sold there first (no inventory risk). Only the rest
  goes to the operator, and only for a token the policy lists.
* Its value is `refPrice × amount`, or else what the plain resting KAS bids of the token pay for it: best first, each up
  to what it has left in this batch, and each only for an amount its own quantity rules accept (at least its minimum
  fill, or all it has left): a bid that could never take the surplus does not value it, whatever it quotes, so a surplus
  below every bid's minimum fill is worth nothing at the bids (value such surpluses with `refPrice`). The best bid's quote
  alone never values a surplus. The value is then cut by
  `haircutBps` and must exceed the fee of the operator's token output (about 205 bytes). It enters the batch's profit
  next to the spread and the tips, so a zero-tip netting whose kept surplus pays its fee is built.
* The batch request names the kept tokens (`keepSurplus`): the builder hands their surplus to the taker (the operator's
  key, one token output carrying `keepCarrier`, 2 KAS by default; `Batch::keepCarrier`) instead of the first pair ask
  buying them. The engine accepts
  the built batch when `change − funding` plus the kept value reaches `--min-profit`, and logs
  `the batch keeps a token surplus as inventory`.
* The matcher only accumulates. The owner sells the inventory off-matcher (its own bot or by hand); the maintenance
  jobs (below) never sell a listed token, whether or not `acceptSurplusTokens` is on, and only merge its UTXOs.

**The kept output's carrier** (owner 2026-10-06: "small enough that there is no penalty"). The carrier is the operator's
own KAS (the accounting counts it; what a smaller carrier does not lock stays in the change), so the only question is the
fee. A token output carries a covenant id, so its KIP-9 storage plurality is 2 and its storage mass is
`4 × 10^12 / carrier` grams. The relay fee does not price storage mass; the storage-inclusive priority fee does once the
storage mass exceeds the batch's fee mass `max(compute, 2 × bytes)`. Measured on the zero-tip TBTC/TUSD keep batch
(`tests/matcher_crossmatch.rs`; KCC20 8/8 tokens, 20,823 bytes, fee mass 41,646, relay fee 0.041646 KAS at every carrier):

| Carrier | Storage mass | Relay fee | Priority fee |
|---|---|---|---|
| 10 KAS | 4,259 | 0.041646 KAS | 0.041646 KAS |
| 5 KAS | 8,231 | 0.041646 KAS | 0.041646 KAS |
| 2 KAS | 20,217 | 0.041646 KAS | 0.041646 KAS |
| 1 KAS | 40,213 | 0.041646 KAS | 0.041646 KAS |
| 0.5 KAS | 80,211 | 0.041646 KAS | 0.080211 KAS |
| 0.1 KAS | 400,209 | 0.041646 KAS | 0.400209 KAS |
| 0.05 KAS | 800,009 | refused: above the block storage limit (500,000) | — |

The smallest batch that can keep a surplus (a KRON / KRON 1 × 1 netting, 11,773 bytes, fee mass 23,546) has no penalty at
2 KAS (storage 20,217) and a higher priority fee at 1.5 KAS (26,881), so the default is 2 KAS. A lower `keepCarrier` (down
to 0.5 KAS) still pays the same relay fee; it only gives the batch a storage-dominated priority mass. Spending the output
later (the owner's sale, a maintenance merge) costs nothing extra: a small input never adds storage mass.

### Economics

A matcher earns, per batch, `Σ bid all-in − Σ ask all-in − network fee`: the crossing spread plus
the makers' optional tips (sompi per whole token). Every KAS amount is the exact covenant value of the leg's fill: a bid pays
`floor(n × (p + tip) / scale)`, an ask receives `ceil(n × (p − tip) / scale)` (rounding favours the makers, at most one sompi per
fill), and the planner never adds a chunk whose rounded amounts lose. Limits are all-in, so makers never pay more (or receive less)
than their limit. The fee is `rate × max(compute mass, 2 × bytes)`, the rate 100 sompi per gram at the floor (higher under
load: "Fee policy" below); measured shapes at the floor
(matcher.md §9, reference 3/3 token unless noted):

| Shape | Bytes | Fee |
|---|---|---|
| 1 bid × 2 asks | 11,753 | 0.0235 KAS |
| armed stop auction fill | 7,377 | 0.0148 KAS |
| repeat take-profit + merge | 11,159 | 0.0223 KAS |

A batch is submitted only when the built transaction's exact profit (`change − funding`) is at
least `--min-profit` sompi (default 1). With zero tips a 0.01 KAS spread per whole token pays a 1 × 2 cross
from about three whole tokens. An update adds about the measured marginal `updateFee` of its token program to the batch's fee
(`kob-protocol/data/keeper_tips.json`: 0.0094 KAS for the KCC-20 programs, 0.0084 KAS for KRON, at 100 sompi/gram) and
earns the order's `keeperTip` (defaults 0.019 KAS, KRON 0.018: twice that fee); the planner charges the cost and counts the
tips as income of the batch, so a crossing too thin to pay its fee alone is still built when the tips of the updates it
enables pay for it, and a zero-tip stop is armed whenever the spread covers its cost. A keeper
earns `refundTip − fee` (defaults 0.030 to 0.125 KAS per program, about twice the fee) per job.

Competing matchers race: the loser's transaction is a mempool double spend (`RejectDoubleSpendInMempool`)
or finds its inputs gone. Both are benign here: the orders involved back off (20 DAA, doubling to
600) and the book is re-read. When the node names the outpoint spent elsewhere and it is an input of one order of the batch
(its order UTXO or its custody: a cancel, a refund, another matcher's fill), only that order backs off; the other orders of
the batch did nothing wrong and are planned again at once. A refusal that names no order's input backs off all of them. The executor never calls `submitTransactionReplacement`: no blind
replacement of anyone's transaction, its own included.

### Fee policy

Every transaction the executor builds (matcher batches, keeper jobs, maintenance jobs, and the x402 facilitator's intent
executions and expiries inside `run`) pays a rate taken from the node's fee estimate (`getFeeEstimate`, rusty-kaspa v2.1.0;
`crates/kob-executor/src/fee.rs`). The node answers three kinds of buckets in sompi per gram: a *priority* bucket
(sub-second inclusion), *normal* buckets (the first: sub-minute) and *low* buckets (the first: sub-hour). Each job takes the
bucket of its urgency:

| Urgency | Bucket | Jobs |
|---|---|---|
| high | priority | matcher batches that fill an IOC, FOK, market or streaming order or a triggered stop (their kill is a minute away); keeper kills; x402 intent executions (the merchant waits) |
| normal | first normal | every other matcher batch (resting fills, arms, trailing ratchets); keeper refunds; x402 intent expiries |
| low | first low | maintenance merges and sales; keeper closes and stray sweeps |

* The rate is `ceil(bucket)`, at least `--fee-rate` (the floor, 100 = the relay minimum) and at most `--fee-max-rate`
  (default 1,000 sompi per gram; a lower value than the floor means the floor).
* `--fee-max-tx-kas` (default 1 KAS) caps the fee of one transaction: one built above it is rebuilt at the rate that fits
  (`rate × cap / fee`), never below the floor (a floor-rate transaction above the cap still goes).
* Profit comes first: a matcher batch is planned at the normal rate (the planner's fee estimates and the profit of every
  allocation use it); an urgent batch is then built at the high rate when its exact profit (`change − funding` of the built
  transaction) stays at or above `--min-profit` there, else at the highest rate that keeps it (never below the normal rate,
  at which it was planned), and at the normal rate when the operator's funding cannot pay the high one. A keeper job is
  built at its urgency's rate when its `refundTip` pays the fee there, else at the highest rate the tip pays, else it is
  unprofitable at the floor as before; a maintenance sale likewise. These prices come from unsigned builds: none is done
  while every rate is the floor.
* The estimate is read at most every `--fee-refresh-ms` (10 s), only by steps that plan; the last good one is used for
  `--fee-max-age-ms` (60 s) while the node answers none, then every rate is the floor (logged once per outage: `no fee
  estimate from the node`). The policy never stops a job, it only prices it. `--no-fee-estimate` pays the floor always (the
  behaviour before the policy).
* Rate changes are logged (`fee rates (sompi per gram)` with high, normal, low and whether they are estimated); every
  `submitted` line carries the transaction's `fee` and `fee_rate`. Metrics: `kob_fee_rate_high`, `kob_fee_rate_normal`,
  `kob_fee_rate_low`, `kob_fee_estimated` (0: at the floor), `kob_fee_estimate_failures_total`.
* The x402 facilitator of `run` prices with the runner's rates (inside the intent's own funds: an execution or expiry the
  intent cannot pay at the higher rate is built at the keeper's floor). The standalone `kob-executor x402` builds nothing at a
  rate of its own beyond the floor; it submits the payers' transactions as signed.
* Why: under the TN10 flood of 10-02 the node estimated 140 to 194 sompi per gram for the normal buckets and 115 for the low
  one while every KOB transaction paid 100; orders, fills and IOC / FOK kills were accepted 2 to 4 minutes late
  (`tools/soak/README.md`).

**Replacement (RBF).** rusty-kaspa v2.1.0 offers `submitTransactionReplacement`, but the executor does not use it: a
transaction is never re-sent at a higher fee. A transaction stuck at a low rate is tracked as pending until it is accepted or
the pending timeout (600 DAA) drops it and its orders back off; the next plan is priced at the rates of that moment. A
replacement would need the same inputs (the matcher's funding UTXO) and a higher fee rate per the node's rules, and must not
replace a competitor's transaction; it is a possible later addition, not part of this policy.

### Keys and funds

The only secret is the operator's hot key (P2PK Schnorr). It is never a command-line argument.
Sources, in order: `--key-file PATH` / `$KOB_OPERATOR_KEY_FILE`, a systemd credential
`kob-operator-key` (`$CREDENTIALS_DIRECTORY`, tmpfs), `$KOB_OPERATOR_KEY` (hex; removed from the
environment once read). A key file holds 64 hex characters and must be mode `0600` or `0400` (the
process refuses group- or world-accessible files; on Windows it warns when another account group can read the file). The secret is wiped from memory on exit.

* Fund the key's P2PK address with a few days of operating KAS only; split it into several UTXOs
  (10 KAS or more each) so independent batches and keeper jobs can run in one tick. Change
  outputs chain within a tick.
* Watch `kob_operator_low_funds` (below `--low-funds-kas`, default 100).

**Funding selection.** The tick orders the operator's spendable P2PK UTXOs by amount (largest first, then outpoint; the change
outputs of the tick's earlier transactions after them). A batch first takes the largest; when the builder refuses it for
`insufficient funds: need N, have H` (every input counted), the batch takes the next UTXOs of the pool in order until the
shortfall `N − H` plus a fee margin for the added inputs is covered, at most `--max-funding-inputs` (8) of them, and is built
again; a batch the first 8 cannot pay waits (`skipped why=lowering: insufficient funds`). Before this release a batch was
handed the largest UTXO only, so a pool of several smaller UTXOs failed batches by 1 to 9 KAS that its total could pay (TN10
soak, 2026-10-01: 10 skips in 30 minutes). Deterministic, and the planner's other choices do not depend on it.

**Consolidation.** While the pool holds more than `--funding-target-utxos` (4) accepted UTXOs, every batch also spends up to
`--consolidate-funding` (2) of the smallest, which its one change output merges (about 0.0003 KAS of fee each at 100 sompi per
gram). A batch whose profit would fall below `--min-profit` with them is built without them. `--consolidate-funding 0` turns it
off. The bank's top-ups and the carriers the maintenance jobs free (below) thus end up in a few large UTXOs instead of many
small ones.

### Maintenance

Protocol v2.6 cross limit routes bought token B in whole ask units and left the remainder on a token output of the operator's
key with a `10 KAS` carrier (`EngineConfig.token_carrier`), one per route; today the planner never leaves a token to the
operator (a pair order's surplus goes to a pair ask's delivery or is sold to KAS bids) unless the opt-in surplus-inventory
policy keeps it (above), but tokens can still reach the operator's key. Nothing spent them: in the TN10 soak
(2026-10-01) each executor held 22 to 28 such UTXOs after 51 minutes (220 to 280 KAS of carriers for a few cents of tokens)
while its spendable funding was about 200 KAS. With the matcher role, the runner now runs the maintenance jobs
(`kob_executor::maintenance`) after the tick's matcher and keeper transactions, over the operator's own key-owned token UTXOs
(the indexer's `token_holdings`, owner = the operator key, live; never an order's custody or stray), per token:

| Job | When | Transaction |
|---|---|---|
| sell | a plain `KobBid` of the token's market (program, extension commitment), live, not spent by a pending transaction or backed off, accepts a fill of what the operator holds (at least its minimum fill, or a fill that ends it), the `--inventory-policy` file does not list the token (surplus inventory is the owner's to sell), and the sale pays more than its fee (`proceeds − fee ≥ 0`) | the operator, as taker, sells as many base units as the best such bid takes (highest all-in per base unit, then age) from up to the program's token inputs; the bid delivery is the bid's; the unsold rest comes back as one token UTXO; the bid's KAS and the freed carriers go to the change |
| merge | otherwise, the operator holds at least 2 token UTXOs of the token | up to the program's token inputs (`Kcc20Ref` 3, 8/8 8, KRON 4; the oldest first) become one token UTXO; the other carriers go to the change (fee only, about 0.01 KAS) |

At most `--maintenance-max-jobs` (2) per tick; their inputs are reserved like every pending transaction's, so the next tick
continues with what is left. KCC-20 jobs pay their fee from the carriers they free; a job on KRON tokens held by address
presence takes the smallest funding UTXO (the presence; its change returns it). Every job is built with the `kob-protocol`
builders (`SendTokens`, a taker `Batch`), signed and validated in the engine, submitted with kind `merge` / `sell` and logged
(`maintenance: the operator's token UTXOs`, with `released`: the carriers it moves back to the funding). A sale runs only on
bids the matcher's tick left: a bid that crosses an ask is the matcher's. `--no-dust-sell` merges only; `--no-maintenance`
turns both off.

**Surplus inventory is not sold here.** A token listed in the `--inventory-policy` file is inventory the matcher
accumulates on purpose; the owner sells it off-matcher. The sale job skips it (`sell: surplus inventory (the owner sells
it off-matcher)`), whether or not `acceptSurplusTokens` is on, and only merges its UTXOs. This also keeps the inventory
away from the sale's rule that any bid paying the fee is good enough: without a floor, anyone could post a `KobBid` far
below the market and receive the holding for little more than the fee. Unlisted tokens (stray dust) are sold as before.

### Node

Run a rusty-kaspa v2.1.0 node with `--utxoindex` (funding lookup), `--retention-period-days=7`,
`--disable-upnp`, RPC on loopback only, never `--unsaferpc`. The executor speaks JSON wRPC
(`--rpc-url ws://127.0.0.1:18210` on testnet-10, `:18110` on mainnet); it checks the network id
and the sync state every tick and does nothing while the node is not synced. Transactions go
through `submitTransaction` (the REST API drops per-input compute budgets). Acceptance: accepted, then final after 100 DAA; a
removed chain block moves its transactions back to pending and they are resent (idempotent). Under
`run` the indexer's follower supplies acceptance (Part A); with `match` / `keep --book-file` the
process follows it itself with `getVirtualChainFromBlockV2` at `Low` verbosity (a second follower
that only knows the ids it submitted).

v2.1.0 relays transactions up to the block limits. Relays running a later release with the
pre-Toccata standard mass cap refuse transactions above ~25 kB: pass `--max-tx-bytes 24000` there
(larger crossings are then split into more chained steps).

### The book

The matcher plans against an `OrderBookView` (validated orders with their custody and placement
record data). In `run` it is the indexer's own store
(`indexer::book::StoreBook`, Part A); standalone it
reads a JSON snapshot (`--book-file`, the `MemoryBook` format of
`kob_executor::matcher::book`), re-read whenever the file changes. The snapshot must contain only
orders the indexer validated (§1.1, §1.2) and must report strays per order. It has no receipts since v2.6 (an old
snapshot's `receipts` key is ignored). `--offline` prints one JSON line per planned transaction: its fills (each with
`evidence`, the leg index of a triggered fill's evidence, else null) and `updates` (`order`, `update` `arm` / `trail`,
`steps`, `evidence`, `take`).

### Running

```
# Plan once against a snapshot, no node, no key needed (ephemeral key, synthetic funding):
kob-executor match --book-file book.json --offline

# Live, but build and validate only:
KOB_OPERATOR_KEY_FILE=/etc/kob/operator.key kob-executor match --book-file book.json \
  --rpc-url ws://127.0.0.1:18210 --dry-run

# Indexer, matcher and keepers in one process, one store (Part A):
KOB_OPERATOR_KEY_FILE=/etc/kob/operator.key kob-executor run --data-dir /var/lib/kob-index \
  --tokens /etc/kob/tokens.json --metrics-file /var/lib/node_exporter/kob.prom

# Matcher and keepers in one process, book from a snapshot file:
kob-executor match --book-file book.json --keep --metrics-file /var/lib/node_exporter/kob.prom

# Keepers alone (a separate host / key is fine):
kob-executor keep --book-file book.json --metrics-file /var/lib/node_exporter/kob-keeper.prom
```

| Flag | Default | Meaning |
|---|---|---|
| `--network` | `testnet-10` | network id the node must report |
| `--tick-ms` | 1000 | tick period |
| `--fee-rate` | 100 | the lowest fee rate, sompi per gram (see Fee policy) |
| `--no-fee-estimate` | off | always pay `--fee-rate` (no `getFeeEstimate`) |
| `--fee-max-rate`, `--fee-max-tx-kas` | 1000, 1 | the highest rate the estimate may set; the most one transaction pays (0: no cap), never below `--fee-rate` |
| `--fee-refresh-ms`, `--fee-max-age-ms` | 10000, 60000 | how often the estimate is read; how long the last one is used while the node answers none |
| `--min-profit` | 1 | sompi per batch after fee |
| `--max-tx-bytes` | 0 | optional cap on the size of a transaction; 0: the physical limit (250 000 bytes of transient mass, the block mass limits of the built transaction). A KaspaCom-template token (25.5 KB program per token input) sweeps its 8 token slots in one transaction; with a 90 000 cap, 3 asks |
| `--no-chain` | off | do not chain onto unaccepted parents |
| `--no-arm` | off | do not arm or ratchet stops with updates (stops the batch fills next to their evidence still trigger) |
| `--tick-budget-ms`, `--max-candidates-per-group` | 20000, 0 | wall-clock budget of a matcher tick; optional cap on the candidates per (book, class, side), 0: all |
| `--keep` (match) | off | run the keepers too |
| `--no-refund` (keep) | off | disable refunds, kills and closes |
| `--sweep-own-strays` (keep) | off | sweep the strays of the operator's own orders IN PLACE (`SweepOrder`, a `SWEEP` record: the order continues unchanged; own-token, token-B and foreign strays, per token within its program's inputs) |
| `--return-foreign-strays` (keep) | off | carry proven FOREIGN strays (tokens other than the order's own) back to the maker inside the refund, kill or close that ends an order (permissionless: no refund path reads another token; after the order ends nothing could move them). A job that would not pay with them is built without them |
| `--pause-file` | none | kill switch: nothing is built or submitted while the file exists |
| `--dry-run`, `--offline` | off | no submission / no node |

A systemd unit for the keyed role:

```
[Service]
ExecStart=/usr/local/bin/kob-executor match --book-file /var/lib/kob/book.json --keep \
  --rpc-url ws://127.0.0.1:18210 --pause-file /run/kob/pause --metrics-file /var/lib/node_exporter/kob.prom
LoadCredential=kob-operator-key:/etc/kob/operator.key
DynamicUser=yes
NoNewPrivileges=yes
ProtectSystem=strict
ReadWritePaths=/var/lib/node_exporter
Restart=always
```

Pausing: `touch /run/kob/pause` (acceptance tracking continues; delete the file to resume).

### Monitoring

`--metrics-file` writes a Prometheus textfile-collector file every tick:

| Metric | Alert when |
|---|---|
| `kob_matcher_last_step_seconds` | older than 60 s (process stuck or node unreachable) |
| `kob_matcher_node_synced` | 0 for more than 5 min |
| `kob_matcher_paused` | 1 unexpectedly |
| `kob_operator_low_funds`, `kob_matcher_funding_sompi` | 1 / below N days of fees |
| `kob_matcher_rejected_total` | increasing (a stale plan or a bug: investigate the logs) |
| `kob_matcher_conflicts_total` | a sharp rise (a faster competitor; consider the fee rate) |
| `kob_matcher_pending_txs` | stays above 0 for minutes (transactions not mined) |
| `kob_matcher_finalized_total`, `kob_matcher_finalized_profit_sompi` | income tracking |
| `kob_matcher_skipped_books` | persistent (unsupported programs, unprofitable books) |
| `kob_fee_estimated`, `kob_fee_rate_high` / `_normal` / `_low`, `kob_fee_estimate_failures_total` | 0 for minutes on a node that should answer (the floor is paid: slow acceptance under load); a rate at `--fee-max-rate` for long (the network is busier than the cap allows) |
| `kob_matcher_book_stale`, `kob_matcher_book_lag_daa` | 1 for minutes (the book lags the node: a catching-up, stalled or `gap` indexer, or a stale snapshot file; nothing is planned meanwhile) |

Logs (`RUST_LOG=info`, default) report every submission, conflict, rejection, acceptance, final
transaction and reorg rollback with its transaction id. A batch that triggers or arms stops logs `the batch triggers or
arms stops` with the number of triggered fills and updates.

### Families

KCC-20 books (reference 3/3, 4/5, 8/8, 16/16, P2) are matched with the `kob-protocol` builders.
KRON books (4 token inputs / 5 outputs per transaction, outputs of 1 to 1e9 units) are planned with the same
planner under their own limits and lowered with the KRON builders through `KronAdapter`
(`kob_executor::matcher::family`). What differs is inside the builders: key-owned KRON tokens (the operator's
own, taker deliveries) are authorised by **address presence** (a P2PK input of the key in the same
transaction, which the operator's funding input provides), every KRON token input carries the next-state
columns of the whole group. The global batch may fill KCC-20 and KRON books, and trigger or arm stops of either, in one
transaction.

The compute budgets of a transaction come from the committed table (`kob_protocol::budget`). An order
covenant's script-unit cost depends on the batch around it (every other custody input in the transaction adds
about 120 units), so the table is generated over every builder shape inside the largest batches the builders
accept, and it is exact: at each role's worst shape one unit less is rejected by the engine
(`kob-protocol/tests/budget_table.rs`). The pair kinds are measured on every program pair and branch (routes of both
sides, netting 1 x 1 and 2 x 2, conditional fills and updates in both evidence modes, if-done fills and re-arms; the pair
shapes of `pair_branch_shapes`), within the builder's limits, which are the planner's (the programs' slots, `MAX_TOK_IN`). The engine validation before submission
remains: should a shape the generator does not produce still come out short (`ExceededCommittedScriptUnits`), the matcher
measures the transaction's inputs in the engine and retries once with the budgets they need (an input's units do not depend
on the budgets around it; `matcher::lower::measured_floors`), the keepers once with one unit of slack; both log a warning and
count it (`budget_slack_retries`); every test asserts the count stays 0.

### Testing

```
cargo test -p kob-executor                                   # scenarios (triggers and updates in batches), keepers, loop, fuzz (60 books)
cargo test -p kob-executor --test executor_e2e            # matcher + keepers on the indexed book, stops armed / triggered in batches, acceptance, reorgs
cargo test -p kob-executor --test flow_kron               # the KRON family through the indexer (every kind, merges, strays, reorgs)
cargo test -p kob-executor --test flow_pair               # pair orders end to end through the indexer: custodies, fills, exits, merges, price rule
cargo test -p kob-executor --test matcher_pair            # pair orders in the batch: routes of both sides, netting 1x1 / 2x2 / 3x1 + route, surplus, FOK / IOC, evidence modes, if-done cycles, random books
cargo test -p kob-executor --test matcher_pair_route      # pair routes: slot limits (3/3, 8/8, KRON), FOK / IOC, exact receipts, price priority, auctions
cargo test -p kob-executor --test matcher_batch           # the global batch: several books + a route + an IOC in one transaction, determinism, splitting, mixed fuzz
cargo test -p kob-executor --test matcher_batch -- --ignored --nocapture   # 10 000 orders per side, timed
KOB_FUZZ_ITERS=5000 cargo test -p kob-executor --test matcher_fuzz -- --nocapture
KOB_TN10_WRPC=ws://HOST:18210 cargo test -p kob-executor --test tn10_node -- --ignored --nocapture
```

Every transaction the tests build is signed and validated in the v2.1.0 engine; the fuzzer asserts
that no plan violates a covenant rule, fills a FOK partially, spends a stray or an order twice,
exceeds 8 token inputs, or breaks the repeat merge rules; every triggered fill and update of every scenario is checked
against the library's own evidence rules (`kob_protocol::build::touch_of`, `Touch::check`; `matcher_common::check_triggers`).
`matcher_scenarios` covers a stop filled next to its evidence and one armed by an update (then auctioned the next tick),
no trigger from a too young, decaying, too small, wrong-side or conditional fill, a thin crossing kept for its updates, a
trailing ratchet, stop entries and a two-family batch; `matcher_pair` pair stops armed (and filled) next to their evidence in both modes. The v2.4 probe
regression is gone with the probes. `executor_e2e` runs the same
engine-validated planning against a book indexed from synthetic chain data (Part A, A.5). Every scenario of the matcher and keeper
suites also runs a **KRON twin** of its book (`matcher_common::to_kron`), and the follower property test runs its
random traffic in both families.
