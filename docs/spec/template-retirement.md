# Templates this build does not pin

Every byte of an order covenant is part of its template hash, so a contract change gives the orders placed afterwards a
new script while every order placed before keeps the old one until it is spent. A build supports only the templates it
pins: an order under any other template (an older one included) is unknown to it, like any other unknown template. The
builders, the CLI and the web wallet build nothing for it, the indexer never lists it nor offers it to a matcher or a
keeper, and a placement record of it is rejected. On chain nothing changes for such an order: its maker ends it with a
raw transaction that spends the order's own `cancel(sig)` entry (custody, strays and the carrier back to the maker), for
example built with the KOB release it was placed with, and its permissionless refund still validates after its expiry.
What this build therefore no longer does for such an order: no executor of this build fills it, arms or trails its stop,
runs its stop-loss exit (an if-done exit placed under an older template is such an order), merges its repeat, or refunds
it as a keeper (its keeper refund and IOC kill wait for someone who builds them with the older release). A maker with a
live stop, exit or resting order under an older template ends it by its cancel and places it again under the current
templates.

## The cancel-only table

The templates of the table below stay in this build in ONE role: their makers' cancel. `kob_protocol::retired` keeps each
retired template's bytecode (`crates/kob-protocol/data/retired/<Kind>.<hash prefix>.bin`, the compiled bytecode of its
artifact at the last commit that carried it, `scripts/extract-retired.mjs`) apart from the pinned set, and recomputes its
template hash from those bytes on load. Every retired template has the state layout and the `cancel(sig)` entry (dispatch tag
`a0893109`) of today's kind of the same name, so its orders' terms decode with today's codec and only their script differs.
The maker cancels such an order in the current app or CLI (`CancelOrder::retired` / `CancelItem::retired`, the signing plan
`retiredCancel`): custody, strays and the carrier back to the maker exactly as its `cancel(sig)` requires, validated for every
retired template in the script engine (`crates/kob-tests/tests/retired_cancel_tests.rs`). **Nothing else is built:** no fill, stop,
trail, merge, sweep, amend, cancel-replace or refund, no listing, and no placement record of it is accepted. The web wallet
resolves the order from its own placement record under the retired script, marks it "older contract version — cancel only", and
counts it in Cancel all; its signer shows such a cancel as "Cancel an order under an older contract version (cancel only)" and
refuses any other spend of it. The indexer's single-order view (`GET /v1/orders/{id}`) serves such an order marked
`cancel_only` (for `kob cancel`); no list, book, count or plan includes it. `kob cancel`, `kob order cancel --view` and
`kob recover --cancel` build its cancel; `kob order sweep`, `kob replace` and `kob refund` refuse it. An order under a template
neither pinned nor in this table is unknown, as above: the web wallet marks it `old-template` with no Cancel button, and its
maker cancels it with the KOB release it was placed with.

## Retired templates

Order templates a build no longer pins, with the build that retired them and why. Every template of these two rows is in the
cancel-only table: the maker cancels it in the current app or CLI; nothing else is built. (The generation before F1, retired by
`6276d5c`, is not in the table: its orders are cancelled with the release they were placed with.)

| Retired templates | Replaced by (bytecode hash prefix) | Why |
|---|---|---|
| `KobBid` b995661f, `KobCondBid` 26042aff, `KobCondAsk` 770314aa, `KobIfdBid` 5740a65d, `KobIfdAsk` afb74446, `KobCondPair` 699c30cc, `KobIfdPair` bb9937a1; the KRON twins `KobBidKron` 6ec1a3dd, `KobCondBidKron` f3181d85, `KobCondAskKron` 63263983, `KobIfdBidKron` b240e0ac, `KobIfdAskKron` f7e04243 and their delivery-0 builds (terminal bid fill returns the spare KAS to the maker) | KobBid dfcd78f1, KobCondBid 07952223, KobCondAsk 23431719, KobIfdBid 956f81e8, KobIfdAsk 2328ea8f, KobCondPair 0d9918cc, KobIfdPair 43c721c5; KobBidKron 7cb4f6ab, KobCondBidKron 8aa82c81, KobCondAskKron e9930d36, KobIfdBidKron 644eeb9c, KobIfdAskKron 2557e020 | The terminal fill of a bid (`KobBid`, `KobCondBid` and the KRON twins) now sends the KAS above the delivery carrier that the order did not spend to its maker as a plain P2PK output when it is at least 1 KAS. `KobCondAsk`, `KobIfdBid`, `KobIfdAsk`, `KobCondPair`, `KobIfdPair` and the other KRON kinds only embed the changed templates as build constants, so their hashes changed with them. A resting bid, conditional order or if-done order under a retired template is not filled, armed, trailed or run by this build: cancel it and place it again. The previous generation (`da358415`: the F1 row below) was retired the same way by this build. The last commit that carries the retired templates is `59d9ea30` (the release before this re-version); this repository no longer carries their sources, only their bytecode in the cancel-only table. |
| `KobCondAsk` 02a34664, `KobCondBid` 28d702e2, `KobIfdBid` cc77a57d, `KobIfdAsk` eb2c6e97, `KobCondPair` 3cae3ae3, `KobIfdPair` 670bbecf; the KRON twins `KobCondAskKron` 27fa0ee2, `KobCondBidKron` 296a2ffb, `KobIfdBidKron` b705e2c3, `KobIfdAskKron` 4797dc30 and their delivery-0 builds (audit 2026-10-29, F1) | KobCondAsk 770314aa, KobCondBid 26042aff, KobIfdBid 5740a65d, KobIfdAsk afb74446, KobCondPair 699c30cc, KobIfdPair bb9937a1; KobCondAskKron 63263983, KobCondBidKron f3181d85, KobIfdBidKron b240e0ac, KobIfdAskKron f7e04243 | A negative `interval` in the state of a trigger-evidence order switched off the `minRestDaa` check of every stop: a stop could be armed by a quote created in the same transaction (`docs/security/AUDIT-2026-10.md` section 12). A resting stop, trailing stop, stop entry or if-done exit under a retired template is not armed, trailed or run by this build: cancel it and place it again. The previous generation (`6276d5c`: 1e1285bf, 2dcca638, 8ba34b33, ce1ccf8e, 98b137f6, a9c04066) was retired the same way by that build. |
