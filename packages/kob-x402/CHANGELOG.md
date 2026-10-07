# @kob/x402 changelog

The package is pre-release (testnet); breaking changes are listed here.

## Unreleased

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
