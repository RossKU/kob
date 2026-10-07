# @kob/x402 changelog

The package is pre-release (testnet); breaking changes are listed here.

## Unreleased

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
