# TN10 upgrade: the pre-mainnet order templates

The switch date is announced separately.

## Before you upgrade

- Every order kind gets a new template. Orders placed before the switch can only be cancelled by their makers
  afterwards: cancel them before the switch, or let them fill. After the switch they are cancel-only; no keeper refunds
  or kills them (the standing decision for retired templates).
- Bots and scripts: update the `kob` CLI and the SDKs from master. Executors and indexers must run the release build:
  `GET /v1/health` reports the new `template_generation` (below).
- The soak's test tokens (TUSD, TETH, TBTC) are issued again with name declarations; their covenant ids change.

## Changed (the order contracts)

- **Native lane only.** Every order refuses a transaction on a user subnetwork lane (KIP-21). No KOB tool builds one; it
  kept the indexer from having to prove such lanes.
- **Evidence is data.** The fill of an ask, a bid or a pair order checks that the order's own state is plain data, and
  their cancels and refunds refuse a signature script shaped like a fill.
  A hand-built order could hide code in a field a stop does not read and make a one-token fill count as any size of
  print, arming or trailing a stop with almost no capital.
- **Custody guard.** A sell order's custody must have a zero borrow guard. A unit someone sent with another guard could
  stand in for the custody and leave the order impossible for the tools to cancel or refund.
- **Empty refund.** A refund needs tokens left in the order. A hand-made empty sell order paid its carriers to whoever
  refunded it.
- **Trail cap.** A trailing sell stop stops at the largest stop the contracts fill (`MAX_STOP`). Past it the stop could
  never fill and the order dropped out of the book.
- A stale comment in the KRON conditional buy is removed (no effect).

## Fees

Order scripts are larger: an ask or a bid by about 0.4 to 0.5 kB, a pair order by about 0.7 kB, every other kind by about
0.1 kB. Fills cost slightly more mass per order. The default keeper tip of a stop is 0.023 KAS (was 0.022) and refund tips
rise by up to 0.001 KAS.

## Operators

- `template_generation`: `266b84d0fe6c5916` (was `2bbbdc9240abaf23`). A build with another value serves other templates:
  every executor and indexer must roll to the release build.
- Orders under the previous templates appear in the cancel-only table (`docs/spec/template-retirement.md`): only their
  makers' cancel is built for them.
