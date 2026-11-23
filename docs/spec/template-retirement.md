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
template hash from those bytes on load. Every retired template has a state layout and the `cancel(sig)` entry (dispatch tag
`a0893109`) of today's kind of the same name, so its orders' terms decode with today's codec and only their script differs. Every
template retired so far was placed before the delivery block of the buy-side kinds (`docs/design/bid-delivery-spec.md` §2), so
its state is the kind's legacy layout: for `KobBid`, `KobCondBid`, `KobPair`, `KobCondPair`, `KobIfdAsk` and `KobIfdPair` the
state without the block (285, 381, 447, 543, 594 and 909 bytes), which decodes with every block field zero (mode 0, what its
script does) and which the cancel encodes back; the other kinds have one layout. The table checks this on load
(`state::layout_of_len` is the legacy layout of the kind).
The maker cancels such an order in the current app or CLI (`CancelOrder::retired` / `CancelItem::retired`, the signing plan
`retiredCancel`): custody, strays and the carrier back to the maker exactly as its `cancel(sig)` requires, validated for every
retired template in the script engine (`crates/kob-tests/tests/kob/retired_cancel_tests.rs`). **Nothing else is built:** no fill, stop,
trail, merge, sweep, amend, cancel-replace or refund, no listing, and no placement record of it is accepted. The web wallet
resolves the order from its own placement record under the retired script, marks it "older contract version — cancel only", and
counts it in Cancel all; its signer shows such a cancel as "Cancel an order under an older contract version (cancel only)" and
refuses any other spend of it. The indexer's single-order view (`GET /v1/orders/{id}`) serves such an order marked
`cancel_only` (for `kob cancel`); no list, book, count or plan includes it. `kob cancel` (also `kob cancel --view`) and
`kob recover --cancel` build its cancel; `kob sweep`, `kob replace` and `kob refund` refuse it. An order under a template
neither pinned nor in this table is unknown, as above: the web wallet marks it `old-template` with no Cancel button, and its
maker cancels it with the KOB release it was placed with.

## Retired templates

Order templates a build no longer pins, with the build that retired them and why. Every template of these four rows is in the
cancel-only table: the maker cancels it in the current app or CLI; nothing else is built. (The generation before F1, retired by
`6276d5c`, is not in the table: its orders are cancelled with the release they were placed with.)

Every template hash a deployment record ever listed is in `crates/kob-protocol/data/retired/record-history.tsv`
(`scripts/record-history.mjs`), and `kob_protocol::retired`'s test ties each to exactly one class (audit 2026-11-23 A3-2):
pinned, cancel-only (the table below), or classified by hand with its evidence:
- **never-deployed**, 22 templates: the branch-only re-pins of 2026-11-16 to 11-18 (the delivery re-pin `68b7b3fa8`..`039a5fa6a`,
  KobBid 0d72813a; DLV-07 `56da43736`, KobBid 1917f8ad; H1 `557f522cd`..`bc2f556f6`, KobBid f5dc8334). No deploy-pages run
  served them (the TN10 site went from KobBid dfcd78f1 to 2f6079ae), nor the soak build or a recorded TN10 run.
- **pre-f1**, 35 templates: the October generations before `6276d5c` (2026-10-06 to 10-19). They predate the first deploy-pages
  run, but the TN10 tests of 2026-10-14/15 placed orders from local builds, so TN10 orders may exist under them; never on
  mainnet. They stay out of the cancel-only table by the October decision above, pending a decision (A3-2).

| Retired templates | Replaced by (bytecode hash prefix) | Why |
|---|---|---|
| `KobBidKron` 7cb4f6ab, `KobCondAskKron` e9930d36, `KobCondBidKron` 8aa82c81, `KobIfdBidKron` 644eeb9c, `KobIfdAskKron` 2557e020 and their delivery-0 builds `KobBidKronD0` bdf2d399, `KobCondAskKronD0` b66e1ca0, `KobCondBidKronD0` 9e2bac11, `KobIfdBidKronD0` c4742e78, `KobIfdAskKronD0` 7ae3d8b7 (bid minimum fill, `71a6c32`) | KobBidKron 4dc2420d, KobCondAskKron 75ad2407, KobCondBidKron 401b2e29, KobIfdBidKron 10c1a47f, KobIfdAskKron 8e030d24; KobBidKronD0 8a7ee676, KobCondAskKronD0 b2b6fe24, KobCondBidKronD0 82fce430, KobIfdBidKronD0 ebc5bff8, KobIfdAskKronD0 daf934fd | A bid fill below `minFill` must take all the buying power left. The KRON kinds of TN10 generation `0506de663e7837a3` that changed with `KobBidKron`; their KCC-20 twins changed with the delivery re-pin (next row), which the same switch to `113cd9b8b119cdaf` deployed. `KobAskKron` did not change. The delivery templates of `1652c9ace` (generation `1b7d75e9b5978ef1`) were never deployed and are not in the table. The last commit that carries the retired templates is `1652c9ac`. |
| `KobBid` dfcd78f1, `KobCondBid` 07952223, `KobPair` c95c9234, `KobCondPair` 0d9918cc, `KobIfdAsk` 2328ea8f, `KobIfdPair` 43c721c5, `KobCondAsk` 23431719, `KobIfdBid` 956f81e8 (bid delivery modes, `0714451`) | KobBid 2f6079ae, KobCondBid aad44664, KobPair d3d775de, KobCondPair 46fd3606, KobIfdAsk 812905a9, KobIfdPair 59bc8aec, KobCondAsk 209cf0da, KobIfdBid 3ea83cf4 | A buy-side order chooses how its fills are delivered (`docs/design/bid-delivery-spec.md`): a new UTXO per fill, accumulated in the order's own custody, or a top-up of the buyer's receive UTXO. `KobBid`, `KobCondBid`, `KobPair` and `KobCondPair` carry the delivery block at the end of their state, `KobIfdAsk` and `KobIfdPair` the block their exits take; `KobCondAsk` and `KobIfdBid` keep their state but embed the longer `KobBid` as a build constant, so their hashes changed with it. The KRON kinds did not change with it (they changed with the minimum fill, the row above). The retired templates have the legacy state layout (above). A resting order, stop or if-done order under a retired template is not filled, armed, trailed or run by this build: cancel it and place it again. The previous generation (`acf39cf`: the next row) was retired the same way. The last commit that carries the retired templates is `d12b28e1`. |
| `KobBid` b995661f, `KobCondBid` 26042aff, `KobCondAsk` 770314aa, `KobIfdBid` 5740a65d, `KobIfdAsk` afb74446, `KobCondPair` 699c30cc, `KobIfdPair` bb9937a1; the KRON twins `KobBidKron` 6ec1a3dd, `KobCondBidKron` f3181d85, `KobCondAskKron` 63263983, `KobIfdBidKron` b240e0ac, `KobIfdAskKron` f7e04243 and their delivery-0 builds (terminal bid fill returns the spare KAS to the maker) | KobBid dfcd78f1, KobCondBid 07952223, KobCondAsk 23431719, KobIfdBid 956f81e8, KobIfdAsk 2328ea8f, KobCondPair 0d9918cc, KobIfdPair 43c721c5; KobBidKron 4dc2420d, KobCondBidKron 401b2e29, KobCondAskKron 75ad2407, KobIfdBidKron 10c1a47f, KobIfdAskKron 8e030d24 | The terminal fill of a bid (`KobBid`, `KobCondBid` and the KRON twins) now sends the KAS above the delivery carrier that the order did not spend to its maker as a plain P2PK output when it is at least 1 KAS. `KobCondAsk`, `KobIfdBid`, `KobIfdAsk`, `KobCondPair`, `KobIfdPair` and the other KRON kinds only embed the changed templates as build constants, so their hashes changed with them. A resting bid, conditional order or if-done order under a retired template is not filled, armed, trailed or run by this build: cancel it and place it again. The previous generation (`da358415`: the F1 row below) was retired the same way by this build. The last commit that carries the retired templates is `59d9ea30` (the release before this re-version); this repository no longer carries their sources, only their bytecode in the cancel-only table. |
| `KobCondAsk` 02a34664, `KobCondBid` 28d702e2, `KobIfdBid` cc77a57d, `KobIfdAsk` eb2c6e97, `KobCondPair` 3cae3ae3, `KobIfdPair` 670bbecf; the KRON twins `KobCondAskKron` 27fa0ee2, `KobCondBidKron` 296a2ffb, `KobIfdBidKron` b705e2c3, `KobIfdAskKron` 4797dc30 and their delivery-0 builds (audit 2026-10-29, F1) | KobCondAsk 770314aa, KobCondBid 26042aff, KobIfdBid 5740a65d, KobIfdAsk afb74446, KobCondPair 699c30cc, KobIfdPair bb9937a1; KobCondAskKron 63263983, KobCondBidKron f3181d85, KobIfdBidKron b240e0ac, KobIfdAskKron f7e04243 | A negative `interval` in the state of a trigger-evidence order switched off the `minRestDaa` check of every stop: a stop could be armed by a quote created in the same transaction (`docs/security/AUDIT-2026-10.md` section 12). A resting stop, trailing stop, stop entry or if-done exit under a retired template is not armed, trailed or run by this build: cancel it and place it again. The previous generation (`6276d5c`: 1e1285bf, 2dcca638, 8ba34b33, ce1ccf8e, 98b137f6, a9c04066) was retired the same way by that build. |

## Lock templates

The automatic creator lock (`KobLockAuto`, `docs/spec/creator-lock-auto.md`) is not an order, and a lock lasts up to 10 years
with no cancel, so the rule above does not apply to it: **no lock template a build ever issued is retired.** Both deployment
records list every one in `locks` (`scripts/lib/deploy.mjs` carries the committed entries over and adds the current template
when it is new; it refuses an entry whose artifact file became another template, so a re-pin keeps the old artifact under a new
name), and it refuses a record that no longer lists a lock template any committed record of the network listed (a hand
deletion). Every build reads each listed template (the genesis owner of the declared terms, so Portfolio and the indexer show the
lock) and releases it (the keeper's and the creator's release spend exactly its script):
`kob_protocol::lock_auto` test `every_issued_lock_template_stays_readable_and_releasable` checks each listed artifact against
the reader. With one issued template the reader is built on that template, so the test is a tripwire: the next lock template
fails it until `AutoLock` carries its template and the readers and the keeper serve each listed one (audit 2026-11-23 R2A-3). A lock re-pin also
changes `template_generation` (below). Issued so far: `KobLockAuto` `ed392654` (2026-11-20).

## Which generation a build serves

`GET /v1/health` reports `template_generation`: the first 16 hex digits of the blake3 hash of the template hashes this build pins
(`kob_protocol::artifacts::PINNED`, in order, then the automatic creator lock's `kob_protocol::lock_auto::TEMPLATE_HASH_HEX`;
`kobd::version::template_generation`), and `cancel_only_generations`: the commits
that retired the templates of the cancel-only table (`acf39cf`, `da35841`, `0714451`, `71a6c32`). The executor's status line ends with the same
generation (`kobd <version> (commit ...), payload v<n>, templates <generation>`) and the metrics carry it as a label of
`kob_build_info`. Two executors with the same generation fill the same orders. A different generation means different order
templates **or** a different lock template: a lock-only re-pin changes it although every order template stays (as the
KobLockAuto entry did for rc2: no longer `113cd9b8b119cdaf`, with no order template changed), so compare the order pins
(`deployment.json` `templates`) before reading a mismatch as missed orders. An executor whose order templates are older than the
release's does not see the orders placed under the new templates (it fills none of them until it is upgraded), and an upgraded
one stops serving the orders of the previous generation (this document): a re-version of the mainnet templates therefore comes
with a published migration window, and operators compare `template_generation` with the release's before and after it
(`docs/ops/release.md`, 7).

## Keeper service for the previous generation: not built (founder decision)

The keepers of this build do not refund, kill or close an order under a retired template (above), and that is decided (audit
2026-11-11 M-13): a template re-version comes with a published migration window instead (`docs/ops/release.md`, 7), within
which makers cancel their resting orders and place them again under the new templates. An order left under a retired template
is never stuck for good: its maker cancels it at any time in the current app or CLI (the cancel-only table above), and at worst
it waits for its own expiry (a GTC order for its 90-day idle limit), after which its permissionless refund validates for anyone
who builds it with the release the order was placed with. A keeper service for retired templates would not be a table entry
like the cancel: a retired template's `refund` and `settle` entries are its own code, and the `BidRefund` retirement changed exactly how a
bid's terminal `settle` pays its maker, which the refund of a bid goes through. A keeper refund of such an order needs, per
retired template, a signing plan for that entry (today only `RetiredCancel` exists, `kob_protocol::tx::SigPlan`), a builder that
pays the outputs the retired code requires (not today's), an ABI and dispatch check of the entry on load (as the cancel's), script
engine tests of every retired template's refund and kill (as `retired_cancel_tests.rs` for the cancel), and the indexer handing
such orders to the keepers as keeper-only. None of it is planned: the maker's cancel is the way out of such an order, and the
permissionless refund still validates on chain for anyone who builds it with the release the order was placed with.
