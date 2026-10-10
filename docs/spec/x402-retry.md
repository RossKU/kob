# Retrying an x402 payment without paying twice

Status: implemented in the TS SDK (`packages/kob-x402`, `KobX402Client` `retry`, `payInvoiceWithIntent` `retry`), the Rust
payer SDK (`kob_x402::client::retry`) and the facilitator (`kob-executor`, `submitRetries`, `rebroadcasts`, the intent
keeper). It applies the conflict rules of [x402-swap-and-pay.md](x402-swap-and-pay.md) section 10 to every profile.

A payment can fail for reasons that have nothing to do with the payer: an order the swap fills is taken by someone else
between the quote and the settlement, the node refuses or loses the broadcast, the facilitator or the merchant does not
answer, finality is not reached within the settle wait, the keeper's execution of an intent loses its orders. The payer
retries, and the retry must never make the payer pay twice.

## 1. Three steps

After an attempt that did not end in a verified settlement, the payer takes one of three steps.

| Step | When | What is sent |
|---|---|---|
| **resend** | the outcome is unknown, or the same signed payment may still go through | the SAME artifact: same payment id, same transaction |
| **rebuild** | the facilitator failed the attempt and released its inputs | a NEW attempt, built from the chain state of the moment |
| **stop** | retrying cannot succeed | nothing |

The step follows the diagnostic (`extensions.kaspa.diagnostic`) and, for an HTTP answer, its status:

| Diagnostic / answer | Step | Why |
|---|---|---|
| no answer, timeout, connection reset; `2xx` without `PAYMENT-RESPONSE`; any `5xx` (but a `5xx` naming a stop diagnostic, e.g. `unauthorized`); `429` | resend | the merchant or the facilitator may hold the payment |
| `settlement_pending`, `node_unavailable`, `rate_limited`, `invoice_pending` | resend | the identical retry is answered from the facilitator's ledger |
| `intent_not_executable` | resend | the dry run failed and nothing was broadcast; the signed creation may execute later |
| `internal` with `retryable: true` (the operator's pause) | resend | |
| `order_conflict`, `order_not_spendable` | rebuild | an order was taken: the transaction can never be accepted; a fresh quote can |
| `invalid_kaspa_exact_utxo` | rebuild | an input was spent or is missing (the anchor check below decides whether that was the payer's own payment) |
| `invalid_kaspa_exact_transaction`, `invalid_kaspa_exact_fee`, `invalid_kaspa_exact_mass` | rebuild | the node refused the broadcast; a rebuild at the current fee rate may pass |
| `expired_authorization`, `expired` | rebuild | a new attempt signs a new authorization |
| `kaspa_payment_identifier_conflict`, `replay`, a policy / limit / offer diagnostic, `intent_expired`, `intent_spent`, `invoice_paid`, `invoice_expired`, anything unknown | stop | |
| `PAYMENT-RESPONSE` that does not verify, a redirect of the paid request | stop | the merchant is not trusted with another send |

The table is `kob_x402::client::retry::{classify, classify_status}`; the TS SDK's `retry.ts` (`classifyFailure`,
`classifyStatus`) mirrors it and `test/retry-wasm.test.ts` checks the two against each other through the wasm build
(`x402RetryDecision`, `x402Diagnostics`) for every diagnostic, both `retryable` values and the statuses a paywall answers with.

## 2. Why no attempt can pay twice

**The anchor.** The first attempt of a payment names its *anchor*: the first of its inputs that is the payer's own (a KAS
coin or a token UTXO of the payer; never an order). Every rebuilt attempt of the payment spends the anchor too. Two
transactions that spend one outpoint exclude each other on chain, so of all the attempts of one payment at most one can
ever be accepted, whatever a merchant later does with the signed attempts it holds and whichever node it uses. The
facilitator's ledger also refuses a second attempt while the first one's anchor is reserved (`replay`).

- A rebuild happens only while the anchor is still an unspent output of the payer (the payer's fresh chain context). An
  anchor that is gone may have been spent by an earlier attempt that was accepted: nothing is rebuilt, the error is
  `payment_pending` / `retry_anchor_spent` (Rust: `GiveUp::AnchorSpent`), and the payer reconciles the earlier attempt
  (an identical re-send returns its settlement if it was accepted).
- The builders choose coins largest first, so a rebuild from an unchanged wallet spends the anchor again. When a larger
  coin arrived meanwhile the TS client builds once more with the payer's outputs that outrank the anchor set aside; a
  rebuilt attempt that still does not spend the anchor is never sent (`retry_unanchored`; Rust: `GiveUp::Unanchored`).
- A first attempt that spends no payer-owned input is only ever re-sent, never rebuilt (Rust: `GiveUp::NoAnchor`).
- An attempt in the mempool still shows its inputs unspent in the UTXO set. A rebuild then conflicts with it in the node's
  mempool and is refused; the attempts stay exclusive and the retry ends within its bounds.

**Per failure.**

| Failure | Step | Double payment excluded by |
|---|---|---|
| An order taken between quote and settle (`order_conflict`) | rebuild from a fresh quote | the dead attempt spends the taken order (it can never be accepted) and the rebuild spends the anchor |
| Facilitator unreachable, timeout, 5xx | resend | the same transaction: the paywall keeps one payment id per transaction (`409 settlement_pending` while one is settling, the cached settlement after), the facilitator resumes or answers an identical retry from its ledger |
| Broadcast refused by the node | rebuild | the facilitator failed the attempt (inputs released); should the refused transaction become valid later, it shares the anchor with the rebuild |
| Accepted, but the answer lost | resend | the same transaction is found settled: `extensions.kob.replayed = true`, the paywall serves it as a replay; nothing is built |
| `paid` finality not reached in time | resend | the facilitator observes the same transaction again |
| Every attempt fails | stop after the bound | the attempts share the anchor: at most one can still be accepted; they stay `rejected` / `pending` in the store and a new `fetch` refuses to sign while they are live (`payment_in_flight`) until revoked (`allowResign` revokes them; attempts that contain an input a revoke already spent are revoked with it) |
| Two retries racing (two calls, two processes) | | in one client paid requests for one resource run one at a time; across processes every attempt spends the anchor (on chain exclusive, refused by the ledger while another is live) |

**Payment ids.** A resend keeps the payment id. A rebuilt attempt takes a FRESH random payment id (from `newPaymentId`;
a caller-chosen `paymentId` names the first attempt only). An artifact store keeps one transaction per payment id and
never overwrites it, the facilitator's ledger refuses another transaction under an id whose transaction is not failed
(`kaspa_payment_identifier_conflict`), and the paywall serves one payment id once: with a fresh id per attempt each
attempt is its own payment and the anchor guarantees that at most one of them is ever accepted, so the resource is served
once. The earlier attempts of a payment that settled are marked `superseded` in the store (the settled one spent their
anchor).

## 3. Limits on every attempt

A rebuilt attempt is built like the first: a fresh chain context (and a fresh quote for a swap), the builder's bounds
(`maxPay` per pay asset, `maxCarrierSompi`, `maxFeeSompi`), the payer's KAS ceiling on everything it takes (amount, carrier
and fee), the preflight, and the spend authorisation. When the offer has no explicit ceiling for its merchant asset (or
for KAS), `approve` is asked again for every rebuilt attempt, at its own cost, with `attempt` and `replaces` set. Asking
on every attempt (rather than only when the new cost exceeds the approved one) is the safer choice: a re-quote can change
what is paid with (`payerSpent`) and the fee as well as the total, and an agent policy sees each signature it authorises.
A refusal ends the retry (`spend_not_authorized`); nothing more is stored or sent.

## 4. Bounds

| | TS `RetryOptions` | Rust `RetryPolicy` | Default |
|---|---|---|---|
| signed attempts (the first included) | `attempts` | `max_attempts` | 3 |
| re-sends of one attempt | `resends` | `max_resends` | 4 |
| backoff | `baseDelayMs`, `maxDelayMs` | `base_delay_ms`, `max_delay_ms` | 500 ms doubling to 8 s, equal jitter (half fixed, half random) |
| overall deadline | `budgetMs` | `budget_ms` | 120 s from the first paid send, capped by the offer's `maxTimeoutSeconds` (an intent: by its own expiry) |

`retry: false` (Rust `RetryPolicy::none()`) is one attempt and no re-send. A step whose backoff would end past the
deadline is not taken. The error of a retry that gave up carries every attempt (`KobX402Error.attempts`:
`paymentId`, `transactionId`, `outcome`, `sends`; Rust `RetryError::signed`, `why`, `outcome_unknown`).

## 5. Intent payments

The payer signs one transaction, the intent's creation; re-planning against the book is the facilitator's job. The SDK
(`payInvoiceWithIntent`, `retry`) re-sends the SAME signed creation while the answer is unknown or the creation may still
execute (`settlement_pending`, `node_unavailable`, `rate_limited`, `intent_not_executable`), within the intent's expiry. It
never signs a second creation on its own: a refused or dead intent is returned (or thrown) as before, and the payer
cancels it or the facilitator expires it on chain.

## 6. Facilitator

- **Submission.** A submit the node could not take (`Unavailable`) is retried with the SAME transaction up to
  `submitRetries` times (default 2, a pause from `pollIntervalMs` doubling, within the settle wait) before the settlement is
  left `ambiguous` for the payer's identical retry. A node that took an earlier try answers `AlreadyKnown`; a refusal
  after an unreachable try is read against the chain first (the earlier try may have been accepted and spent the inputs
  itself) and is then a broadcast, not a failure. Direct payments and intent creations alike.
- **Re-broadcast.** While a settle observes a transaction that left the mempool without being accepted (an eviction, a
  node restart) it broadcasts the same transaction again, up to `rebroadcasts` times (default 2); a refused re-broadcast
  ends them and the observation goes on (an order spent meanwhile is still `order_conflict`).
- **Intents.** The keeper plans each execution against the book of the moment, records it before it submits it, and when
  an order it named is taken (a submit conflict, or the order spent while the execution is not accepted) or the execution
  vanished (a submit with an unknown outcome that never reached the mempool), marks it dead, remembers the lost orders and
  plans again, at most `intents.maxAttempts` executions and never past the intent's deadline. Every execution spends the
  intent's UTXO, so at most one can be accepted; an execution whose answer was lost but that reached the node is waited
  for, not replaced.
- Metrics: `kob_x402_submit_retries`, `kob_x402_rebroadcasts`, `kob_x402_intent_executions`, `kob_x402_intent_conflicts`.

## 7. Tests

- `packages/kob-x402/test/retry.test.ts`: lost order race then success (fresh quote, anchor, `approve` per attempt, served
  once); facilitator 5xx, dropped connection and client timeout then success (one transaction); broadcast refused then
  success; payment accepted but the answer lost (re-sent, settled once, served as a replay); every attempt failing (gives
  up after 3, the attempts share the anchor, a new fetch is refused); refusals not retried; a worse re-quote refused by
  `approve` and above the KAS ceiling; a spent anchor and a larger coin; concurrent paid requests and a racing `resume`;
  intent creations re-sent, never re-signed. `test/retry-wasm.test.ts`: the TS and Rust tables agree.
- `crates/kob-x402/src/client/retry.rs`: the driver and the table.
- `crates/kob-executor/tests/x402_retry.rs`: the Rust driver against the facilitator over the mock chain, the same failures
  injected, and two retries of one payment racing (one pays, the other is refused or finds the anchor spent).
- `crates/kob-executor/src/x402/facilitator_tests.rs`: re-submission after an unreachable node, a refused repeat of a
  transaction that went through, re-broadcast after an eviction. `crates/kob-executor/tests/x402_intents.rs`: the keeper
  re-plans against the book of the moment after losing an order; an execution with an unknown submit outcome is never
  executed twice.
