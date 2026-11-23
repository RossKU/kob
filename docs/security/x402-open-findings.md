# x402: open findings (experimental, outside the launch review)

**x402 is experimental.** x402 (the `kob-x402` crate, the facilitator in `kobd`, the client SDK `packages/kob-x402` and
`docs/spec/x402-*.md`) is outside the mainnet launch review of 2026-11-23 (`REVIEW-2026-11-23.md`, *Not covered*) and has no payment UI
in the web app (it only has a read-only watch of an invoice's status, in the notification panel); it gets a dedicated audit of its own before it is recommended for real funds or its KCC-20 profile is proposed
as a standard (founder decision, 2026-11-23).

The launch review still read parts of x402 on the way (areas B, D, A, G and the second-round tools pass). This page keeps what it
found and left open, so the dedicated audit starts from it. Ids are those of the review's reports.

**Level.** *Profile*: a property of the protocol as specified (`docs/spec/x402-kcc20-profile.md`, `x402-swap-and-pay.md`,
`x402-retry.md`): what a payment may contain, what finality, a deadline or an expiry mean, replay and domain separation. Any
other implementation of the profile inherits it, so these matter most before the profile is proposed as a standard.
*Implementation*: a property of KOB's code (the facilitator, the verifier, the SDKs), fixable without changing the profile.
*Both*: the profile leaves the point open or unsafe, and this implementation does not close it.

| Id | Severity | Level | Finding | Status |
|---|---|---|---|---|
| X-1 | Medium | Implementation | An anonymous payer could grow an invoice record without limit (pay, replace the payment back to itself, wait 60 s, repeat), costing disk, CPU and node queries | Fixed (`1610f6d45`: released payments count toward a bound); the fallback opened R2-X1 |
| R2-X1 | Low | Implementation | After 16 released payments, anyone who knows the invoice id can hold the invoice pending for one hour per cycle, and the releases after that are not watched, so a late-accepted payment could let the invoice be paid twice | Open. Fix: count released payments per payer, not per invoice; keep watching every release |
| X-2 | Medium | Both | Payment facts and finality (the payer's inputs, the merchant output, acceptance) come from one node; a lying node makes the facilitator answer success and the merchant delivers for nothing. The settlement response does not say which node answered | Partly closed: the profile and `docs/ops/executor.md` say "own node only"; the code still accepts `node: "resolver"` and the public pair. Fix: refuse them for x402 without an explicit unsafe flag, or require the cross-checked acceptance for finality |
| R2-X2 / A3-4 | Low | Profile | A `standard-native` payment may be signed with any consensus sighash type (`NONE`, `SINGLE`, `ANYONECANPAY` variants), and the facilitator verifies and broadcasts it. Under `NONE` a mempool observer can rebuild the outputs and take the payer's input; under `SINGLE` the change. KasWare and Kaspire pass the request's `sighashType`. KOB's own signers sign `ALL` only | Open (PLAUSIBLE: the v2.1.0 replacement rules were not checked). Fix: the profile allows only `ALL` (`0x01`, `0x81` at most); the facilitator refuses the rest |
| B3-I4 | Info | Profile | The expiry of an `exact`, KCC-20 and swap payment is off chain only: nothing on chain stops a signed payment after its deadline | Documented, with mitigations; the standard proposal should state it as a property |
| B3-I5 | Info | Profile | The native authorisation digest is SHA-256 over canonical JSON with no tagged-hash domain | Open (no practical attack found). Fix: a domain tag in the profile before it is proposed |
| X-3 | Low | Both | An intent that only the payer's own order can fill passes the admission dry run; the payer cancels that order, and every other payment of the invoice gets `invoice_pending` until the intent's deadline, repeatedly | Open (PLAUSIBLE). Fix: give up early after N seconds with no buildable execution; the profile should bound how long an intent may hold an invoice |
| L13 | Low | Both | The intent verifier does not check the creation's lock time (native and swap do); a future lock time costs at most a pending hold, like X-3 | Open. The profile should state the lock-time rule for every kind |
| A3-5 | Info | Profile | Router costs that go to others: after the deadline anyone keeps up to 0.1 KAS (`EXPIRE_MAX_FEE`); on a token intent's fill the keeper keeps the intent's KAS (`keeper_value`, 1 KAS by default) and the lock carrier. A wrong token handle freezes a lock for good (guarded only by the payer's client) | Open; counted in the client's `worst_kas`. The profile should state who may keep what |
| B3-L3 | Low | Both | Swap-and-pay: the quote comes from an untrusted order source and its price is never checked; a lying source routes the swap into its own orders, bounded only by `maxPay` (the merchant still gets exactly its amount) | Open. Fix: an optional `maxPrice` or reference hook; at minimum the profile says `maxPay` is the whole slippage bound |
| B3-I3 | Info | Implementation | Swap: the quote's outpoints are not re-read on the node before signing | Open (no payer loss found) |
| X-4 | Low | Implementation | Spend watches live only in memory: after a restart a paid intent can end as `intent_spent`, a late execution as `spent` instead of `executed_late`, and a full watch table ends an expiry `spent` at once | Open (PLAUSIBLE). Fix: an unknown spender stays unknown; check `watch_spend`'s result |
| X-5 | Info | Implementation | The E-L1 fix leaves a race of about 10 s (the follower's lag allowance) | Open (PLAUSIBLE) |
| X-6 | Info | Profile | `x402-swap-and-pay.md` §17.7, §18.4 and open question 9 do not describe `executed_late` | Open (spec text) |
| L12 | Low | Implementation | The facilitator's ledger silently stops writing after a failed compaction; payments live only in memory until a restart, and invoice and attempt history are lost | Open. Fix: mark the ledger broken when the reopen fails |
| L14 | Low | Implementation | A `pending` intent entry executes but cannot reach `accepted`; only the payer's identical retry moves it | Open (PLAUSIBLE) |
| L15 | Low | Implementation | Every anonymous intent dry run reads the whole book (no cache) | Open |
| L16 | Low | Implementation | One shared `/verify` map of 4,096 entries: one merchant's traffic evicts another's records, whose `/settle` then waits for the chain | Open |

Counts: Medium 2 (X-1 fixed, X-2 partly closed), Low 10, Info 6. Profile or both: 9.

## What the dedicated audit must cover

The launch review did not read these (the *Not covered* lists of its reports); the audit covers them, and the profile-level rows
above, before x402 is recommended for real funds or proposed as a standard:

- `kob-x402/src/swap.rs` (the swap-and-pay verifier) beyond its amount checks.
- `wire.rs` beyond finality, `policy.rs`, `canonical.rs`, `safe_tx.rs`.
- The intent execution planner and builder (`intent.rs` from line 860 on, `kob_protocol::build::execute_intent`), where an
  execution's residue and fillers go (`KeeperParams.keeper`), and whether the payer's unsold lock always returns to the payer.
- The client bodies `client/native.rs`, `client/token.rs`, `client/swap.rs`, `client/intent.rs` (with `revoke_*` and
  `cancel_intent`), and `client/retry.rs` beyond its header (the anchor argument was taken on trust).
- The facilitator's settle and reconcile loops (`kobd/src/x402/facilitator.rs`), `facilitator/intent.rs` (X-3, X-4 were not
  re-read in the second round), `http.rs`, `ratelimit.rs`, `quote.rs`, `node.rs`.
- The TypeScript SDK (`packages/kob-x402`) and the `kob-wasm` x402 bindings beyond what the web-signing pass read.
- The specifications `docs/spec/x402-*.md` as a whole, against the code and against the upstream binding
  (`elldeeone/kaspa-x402`, `kaspa-exact-v2`): sighash types, finality, deadlines and expiry, replay and domain separation, the
  intent's lock time and who keeps what.
- The Kaspa v2.1.0 mempool replacement rules (they decide R2-X2).
- Engine runs and tests: no x402 test was run by the review; every CONFIRMED above is by reading.
