# @kob/x402 changelog

The package is pre-release (testnet); breaking changes are listed here.

## Unreleased

### The Kaspa x402 binding v1.0.0 (clean cutover from v1.0.0-rc.1)

Aligned with elldeeone/kaspa-x402 v1.0.0 (tag `v1.0.0`, commit `e5f5b0e`). There is no rc.1 compatibility: payers,
merchants and facilitators move together (**breaking**, wire, Rust and TypeScript).

- **`extra.paymentFlow: "upfront"`** on every exact offer (the binding requires it; an omitted flow, `authorization` or
  `escrow` is invalid). Every offer builder emits it (`native_requirements`, `kcc20_requirements`, `swap_requirements`,
  `intent_requirements`, the wasm builders, `buildOffer`), including KOB's own profiles (kcc20, swap-and-pay, intents: their
  resource is likewise served only after the settlement is accepted). Verifiers refuse an offer without it
  (`invalid_payment_requirements` / `invalid_kaspa_x402_payload`), payers skip it (`classifyOffer`, `select_native_offer`).
  Requirements hashes, request hashes and invoice ids change with it.
- **Request authorization v2**: the binding's Schnorr-signed digest is `kaspa-x402-exact-request-authorization-v2`, whose
  preimage adds `paymentIdentifier` (`paymentPayload.extensions["payment-identifier"].info.id`, verbatim). KOB's payload
  commitment (kcc20, swap-and-pay, intents) is `kob-x402-payload-commitment-v2` and binds the identifier the same way. v1
  authorizations are refused. The payment identifier is mandatory for every exact payment (`Policy::require_payment_identifier`
  is gone; `Verified::payment_identifier` is a `String`); payers choose it before signing (`common::resolve_payment_id`;
  `PreparedSwap::payment_identifier` / `PreparedIntent::payment_identifier` are a `String`). The wasm digest bindings
  (`x402SignedAuthDigest`, `x402PayloadCommitDigest`) take `paymentIdentifier`.
- **Signature hash types**: a standard-native payment accepts each payer signature's encoded consensus-supported hash type
  (ALL, NONE, SINGLE, each with ANYONECANPAY: `sighash::CONSENSUS_HASH_TYPES`), as the binding's 1.0.0 verifiers do; any
  other byte is refused. KOB's signers keep SIGHASH_ALL, and KOB's own profiles keep accepting SIGHASH_ALL only (module
  `sighash`).
- **Payer funding policy** (`KobX402ClientOptions.policy`, `FundingPolicy`): an unattended payment needs a complete policy
  (`fundingSource`, `allowedOrigins`, `allowedPayTo`, `allowedKinds`) plus a `capabilities.maxAmount` ceiling for the merchant
  asset; anything less, a ceiling alone included, is paid only when `approve` says yes (new reason `incomplete_policy`).
  Every pinned list is enforced whether `approve` is set or not: a request to another origin is refused before anything is
  sent (`policy_violation`), an offer to another recipient or of another kind is skipped. The origin and the resource come
  from the actual request URL: a 402 whose `resource.url` names another resource is refused (`invalid_payment_required`).
- Conformance vectors: `crates/kob-x402/vectors/kaspa-x402-v1.0.0/` (byte-identical upstream blobs, now with LF line
  endings; adds `negative/exact-payment-flow-missing.json` and `sighash/consensus.json`) replaces `kaspa-x402-rc1/`.
  The golden vectors (`vectors/golden/x402-payments.json`) are regenerated.

### Facilitator: an execution accepted while the keeper reads the intent is the payment

- kob-executor intents: an execution that left the mempool without being accepted yet (inside a block not merged) was marked
  dead; when it was accepted before the keeper read the intent's UTXO, the intent was gone and the payment was answered
  `intent_spent` and failed for good, though the merchant was paid. Before giving up on a spent intent the keeper now checks
  every execution of its own for acceptance and waits for its finality.

### Facilitator: a refused repeat submit with an unreadable chain stays unknown

- kob-executor: when a submit with an unknown outcome is repeated and the node refuses the repeat, the facilitator reads the
  chain to tell whether the first try went through. A chain read that failed counted as "not seen", so a payment the node had
  taken was marked failed, its inputs released, and the payer told `invalid_kaspa_exact_utxo` / `invalid_kaspa_exact_transaction`
  (a rebuild). It is now `node_unavailable` (ambiguous, the inputs stay reserved), for direct payments and intent creations.
- kob-executor: `GET /health` (`settle_health_cached`, on an async worker) no longer waits for a node probe in flight: the
  probe of `/supported` held the cache's lock across its node call (up to the node timeout). The probe is single-flight on
  a lock of its own; the cache is locked only to read or write it.

### Payer: an expired artifact stays live while its inputs are unspent

- `KobX402Client` (a new `fetch` / `payInvoice` for a resource with an earlier signed, pending or rejected artifact): an
  artifact whose authorization expired was no longer counted as live, so a new payment was signed beside it without a revoke.
  Expiry only bounds when a facilitator settles; the transaction stays valid on chain (x402-kcc20-profile.md 9.4), and a
  merchant holding it could broadcast both. An expired artifact now stays live (`payment_in_flight`, or revoked first with
  `allowResign`) until one of the payer's inputs it spends (its anchor, else every input that is not an order of its route) is
  gone from the payer's chain context.

### Facilitator: `/supported` advertises only what it can settle now

- kob-executor: `GET /supported` is empty while the facilitator is paused (as before) or its node does not answer a probe
  (`getBlockDagInfo`, cached for `FacilitatorConfig::health_cache`, default 2 s, one probe at a time, off the async
  workers), and lists the intent mode (`kob-intent-v1`, `router`) only while the book it executes against is ready
  (`Facilitator::with_book_readiness`; inside `kob-executor run` the indexer caught up or within its lag tolerance, which
  also gates the intent executions' book). `GET /health` adds `degraded` (from the last probe, no chain access).
  `Facilitator::settle_health` / `settle_health_cached`, `SettleHealth`, `BookReadiness`; `x402::prepare_with` and
  `executor::x402_service` take the readiness / the indexer's health (**breaking**, Rust).

### Rust payer: the retry driver pays intents (re-send only)

- `kob_x402::client::retry::pay_intent_with_retry(policy, max_timeout_seconds, deadline_ms, creation, transport)` over a
  `ResendTransport` (send, clock, sleep, jitter: nothing to build): the one signed creation (`IntentPayment::attempt()`) is
  re-sent while its outcome is unknown or it may still execute, within `max_resends` and the shorter of the budget, the
  offer's `maxTimeoutSeconds` and the intent's deadline (`IntentPayment::deadline_ms()`); it is never rebuilt: a `rebuild`
  diagnostic ends it with the new `GiveUp::NoRebuild` (cancel the intent or let it expire), a refusal with `Stopped`.
  **Breaking (Rust):** `GiveUp` gains a variant.

### Payer: invoices are paid under the client's limits and their settlement is verified

- `KobX402Client.payInvoice(fetchedInvoice, invoiceClient, { acceptIndex?, paymentId? })` pays any entry of an invoice
  (native, kcc20, swap-and-pay, intent: the best one the payer can pay, ranked as for a 402) with the invoice id as the
  request hash, under the same limits as a paid request (`capabilities.maxAmount`, `maxPay`, `approve`, `intentTerms`, the
  preflight; an intent's lifetime never runs past the invoice's expiry). The payment is stored before it is submitted
  (`ArtifactRecord.url` is the invoice's pay URL, `method` `POST`), re-sent while its outcome is unknown (`settlement_pending`,
  `invoice_pending`, `node_unavailable`, `rate_limited`, no answer, a 5xx), rebuilt after a failure that released its inputs
  (anchored, a fresh payment id; never an intent creation), and the settlement must verify. Returns an
  `InvoicePaymentReceipt` (`invoiceId`, `acceptIndex`); a refusal (`invoice_paid`, `invoice_expired`, ...) throws
  `payment_failed`, a payment still pending at the end (an intent being executed) `payment_pending` (follow the invoice
  status).
- `verifyInvoiceSettlement(settlement, invoice, payment)`: the payment was built for the invoice (request hash = its id) and
  one of its entries, and the settlement is a success for exactly it (its transaction, or an intent's creation), with the
  entry's amount, network and a kcc20 entry's custody class.
- **Breaking (behaviour):** `payInvoiceWithIntent` verifies a success with `verifyInvoiceSettlement`; one that does not
  verify (another creation, another amount or network) throws `invalid_settlement` with `error.payment` set instead of being
  returned.

### Payer: intent offers (`kob-intent-v1`) are paid by `KobX402Client`

- `KobX402Client` pays an intent offer of a paywall: it signs ONE creation of a router intent on the payer's own terms
  (`intentTerms(request)`: `maxSell`, or `maxPay` / `maxExtra`, optionally the actor, `lockAmount`, `keeperValue`,
  `expiresInMs`; the client's fee rate), locally or through the wallet (`prepareIntent` / `finishIntent`). Without
  `intentTerms` (or when it answers `undefined`) and without a `maxPay` bound for the pay asset an intent offer is not paid.
  The worst case (`IntentPayResult.payerSpent`) must be within the bound (`preflight_failed` / `overpayment` otherwise,
  nothing stored or sent) and is what `approve` is shown (`cost.payerSpent`). Intent offers rank after the payer-signed
  routes; `capabilities.allowIntent: false` (or `allowSwap: false`) skips them. Only the router the SDK recognizes
  (`ROUTER_ARTIFACT_ID`, a critical route) is classified (`classifyOffer` kind `intent`; `OfferKind` gains `intent`).
- The PAYMENT-RESPONSE of an intent payment must name the signed creation in `extensions.kob.intent.creation` (its
  `transaction` is the execution): `verifySettlement(..., intent)`, `verifyRecordSettlement`. `settledRequestTransaction`
  moved to `client.ts` (still exported from the package).
- A creation is re-sent while its outcome is unknown or it may still execute and never re-signed: a `rebuild` diagnostic
  ends the retry; the retry deadline is also capped by the intent's expiry. The intent handle is stored with the signed
  creation (`ArtifactRecord.intent`) and set on the error (`KobX402Error.intent`): cancel or expire the intent with it.
- `PaymentReceipt.settledTransaction` (the transaction that paid the merchant: an intent's execution) and
  `PaymentReceipt.intent`.
- `x402PayIntent` / `x402FinishIntent` (kob-wasm) report `kasSpent`: the most KAS the creation takes from the payer's coins
  (the intent's KAS, the creation fee and a token intent's lock carrier).

### Swap-and-pay: the payer's bound is required everywhere

- **Breaking (wasm and Rust API):** a swap is never built without the payer's bound on what it costs, as `KobX402Client`
  already required (`maxPay`). `x402PaySwap` / `x402PrepareSwap` refuse a request without `options.maxPay`
  (`invalid_kaspa_x402_payload`, "no unbounded swap"); the Rust `prepare_swap` / `pay_swap` refuse
  `SwapOptions::max_pay: None` and the payer's `preflight_swap` takes `&PayBound` (no `Option`). `x402Preflight` checks
  `maxPay` when given and otherwise only verifies (a merchant-side check). `Quote::pay_asset()` names the asset the bound
  must count. The golden vectors carry the bound (`KOB_REGEN=1`); no transaction byte changed.

### Native payments: the payer cap is required (audit A1-3)

- **Breaking (wasm API):** `x402PayNative` (`kob-wasm`) requires `options.maxAmountSompi`, the most sompi the payer pays for
  the offer, like the Rust `PayOptions::new`; a request without it is refused (`invalid_payload`) instead of defaulting to the
  offer's own amount. `PayRequest.maxAmountSompi` carries it, and `payNative` of `createKobWasm` refuses a request without it
  (`bad_request`). `KobX402Client` passes `capabilities.maxAmount.KAS` when set, else exactly the offered amount (the one
  `approve` is asked about); no change for its callers.

### Payer: a failed payment is retried without paying twice

- **Breaking (behaviour):** `KobX402Client` retries a payment inside the same `paidFetch` (`retry`, default
  `{ attempts: 3, resends: 4 }`, 500 ms backoff doubling to 8 s with equal jitter, deadline the shorter of 120 s and the offer's
  `maxTimeoutSeconds`; `retry: false` restores one send). An unknown outcome (no answer, a timeout, a 5xx, a 2xx without
  `PAYMENT-RESPONSE`, `409 settlement_pending`, `node_unavailable`) re-sends the SAME stored artifact; a failure that left the
  payer's funds where they were (`order_conflict`, `order_not_spendable`, `invalid_kaspa_exact_utxo`, the node refusing the
  transaction, `expired_authorization`) rebuilds the payment: a fresh chain context and quote, the limits, the preflight and
  `approve` again (asked for every rebuilt attempt, `PaymentApproval.attempt` / `replaces`), a fresh payment id, and the
  payment's anchor (the first input of the first attempt the payer owns) spent by every attempt, so two attempts of one payment
  can never both be accepted. A rebuild is refused when the anchor is no longer unspent (`payment_pending` /
  `retry_anchor_spent`) or the rebuilt attempt does not spend it (`retry_unanchored`). Anything else stops.
  See `docs/spec/x402-retry.md`.
- `PaymentReceipt.attempts`, `.resends`, `.superseded`; `KobX402Error.attempts` (every attempt with its outcome when the retry
  gave up); `ArtifactRecord.attempt`, `.replaces`, `.anchor` and the status `superseded` (an earlier attempt of a payment that
  settled). `PaymentApproval.attempt` is set (1 for the first attempt).
- Paid requests for one resource (method + URL) run one at a time in a client; `resume()` waits for a running payment of the
  same resource and re-sends within the `retry` bounds (it never rebuilds). The `allowResign` revoke of live attempts revokes an
  attempt whose input an earlier revoke spent together with it.
- `payInvoiceWithIntent(..., { retry })` re-sends the same signed creation while the answer is unknown or the intent may still
  execute (`settlement_pending`, `intent_not_executable`, ...), within the intent's expiry; it never signs a second creation. It
  returns `sends`.
- `retry.ts`: `classifyFailure`, `classifyStatus`, `retryPolicy`, `backoffMs`, the same table as the Rust
  `kob_x402::client::retry` (checked against the wasm build's `x402RetryDecision` / `x402Diagnostics`; `KobWasm.retryDecision`,
  `KobWasm.diagnostics`).
- Facilitator (kob-executor): a submit the node could not take is retried with the same transaction (`submitRetries`, default 2)
  and a transaction evicted while observed is broadcast again (`rebroadcasts`, default 2).

### Paywall: confirmed finality by default

- **Breaking:** `PaywallConfig.finality` defaults to `confirmed` (was `accepted`): the offers ask for it and the settlement must
  show at least the offer's finality in `extensions.kaspa.finality`, or the paywall answers `502` and does not serve. `accepted`
  serves at depth 0, a few seconds sooner, while a payer can still race a conflicting spend through another node.
- The facilitator (kob-executor) answers at the stronger of the offer's finality and its `minFinality` (default `confirmed`) and
  reports `extensions.kob.confirmationsDaa`.

### The vendored SDK is verified when it is loaded

- `loadKaspaNodeSdk(dir, { expectedTreeSha256? })` hashes the directory before it loads anything and refuses a build that is not
  the pinned v2.1.0 SDK (`KASPA_NODE_SDK_TREE_SHA256`, `sdkTreeSha256`); `expectedTreeSha256: null` loads an unpinned build on
  purpose. `fetch-sdk` checks its pin only when it runs.
- `PaywallConfig.maxSettling` (default 1000): payments being settled are held apart from settled ones; beyond it a request is
  answered 503 (retryable).

### Paywall: intent payments are bound to their creation

- An intent payment (`kob-intent-v1`) carries the intent's creation and is settled by the facilitator's execution. The
  paywall now compares the request's declared transaction id with `extensions.kob.intent.creation` of the settlement
  (`settledRequestTransaction`), not with `transaction` (the execution), so a paid intent is served instead of `502`.
  A settlement naming another creation, or none, is still refused. `PaidContext.transactionId` is the execution.

### A settled payment is served only to the request that carries its payment id

- **Breaking:** a settled transaction presented under another payment id is no longer served as a repeat of the first
  payment: the paywall answers `409` `kaspa_payment_identifier_conflict` while it remembers the transaction, and the
  facilitator (kob-executor) refuses it with the same diagnostic from its durable ledger (also after either one
  restarts). A broadcast transaction is public; its payment id never reaches the chain, so it is what names the payment.
  The payer's own retry (same id, same transaction) is still served, with `replayed: true`.
- The facilitator records the first success answer of a payment in its ledger and marks every later one with
  `extensions.kob.replayed = true`; the paywall passes it on as `PaidContext.replayed`, so a restarted paywall hands
  the handler the payer's retry as a repeat, never as a new payment. A facilitator that does not give this mark leaves
  `replayed` to the paywall's own memory.
- Facilitator: a transaction settled without a payment id (only an embedded facilitator whose `Policy::require_payment_identifier` is off) is answered
  only by the settle that settled it.

### Paywall: a paid response is replayed only to the payment that settled it

- The paywall remembers, per payment id, the transaction the payment was settled with (its declared id and the
  SHA-256 of its transaction text). A request repeating a remembered id is answered from memory only when it carries
  that same transaction; any other transaction (or none) under the id is refused with `409`
  `kaspa_payment_identifier_conflict` and is not forwarded to the facilitator.
- **Breaking:** the paywall requires the payment's transaction to declare its id (safe JSON `id`, 32-byte hex); a
  payload without it is answered with a corrective `402` (`invalid_kaspa_x402_payload`). The SDK builders always
  declare it. A settlement is served only when its transaction id is the declared one.
- `PaidContext.replayed` is true only for a repeat of the same signed transaction.
- The facilitator (kob-executor) answers another transaction under a bound payment id with
  `kaspa_payment_identifier_conflict` instead of the bound transaction's settlement; the Rust payers
  (`kob_x402::client::{native, swap, intent}`) draw a random payment id by default (`random_payment_id`;
  `derive_payment_id` is removed).

### Payer: swap bounds are counted per pay asset

- **Breaking:** `KobX402ClientOptions.maxPay` (new) is a per pay asset bound: `{ KAS: '<sompi>', '<covenant id>': '<base
  units>' }`. A swap pays only with an asset that has a bound: the first of the offer's pay assets the payer can pay and
  has bounded, whatever order the merchant lists them in.
- `maxPayAmount` stays as a KAS-only shorthand (`maxPay: { KAS }`); it is never read in the units of a token.
- `PreflightRequest.maxPayAsset` (new) names the asset `maxPay` counts; the wasm bindings (`maxPayAsset` next to `maxPay` in
  the swap options and in preflight) refuse a payment whose pay asset is another one (`pay_asset_not_accepted`). A bare
  `maxPay` counts KAS.
- Rust: `SwapOptions::max_pay` and `preflight_swap` take a `PayBound { asset, amount }` (`PayBound::kas`, `PayBound::token`).

### Payer: approval sees the cost; a swap needs a bound

- **Breaking:** `approve` is asked after the payment is built and preflighted (and before it is stored or sent), with
  `cost: PaymentCost` (merchant asset and amount, the swap's pay asset and `payerSpent`, the network fee). A refused
  payment is discarded; the next ranked offer is tried.
- **Breaking:** a swap-and-pay offer is never built without a `maxPay` bound for its pay asset; `approve` no longer
  stands in for it (the `no_max_pay` reason is gone).

### Payer: carrier and network fee count against the limit

- `PaymentCost` (what `approve` sees) carries `carrierSompi` and `kasSpent`: everything the payment takes from the
  payer's KAS (a native amount, the carrier funded into a merchant token output, the network fee; a KAS-paid swap's
  whole cost).
- **Breaking:** `kasSpent` is checked against the payer's KAS ceiling (`maxPay.KAS` / `maxPayAmount`, else
  `capabilities.maxAmount.KAS`) after the payment is built and before it is stored or sent: a native offer at exactly
  the ceiling is no longer paid when the fee would take it past it. Without a KAS ceiling, a token payment's KAS needs
  `approve` (reason `no_kas_cap`).
- `PayResult.kasSpent` (new, swap-and-pay builders): sompi taken from the payer's KAS coins.
- Rust: `PayOptions::max_total_sompi` (native: amount + fee), `Kcc20Options::max_kas_sompi` (carrier + fee),
  `SwapOptions::max_kas_sompi` and `PreparedSwap::kas_spent` / `SwapPayment::kas_spent` (KAS out of the payer's coins).
