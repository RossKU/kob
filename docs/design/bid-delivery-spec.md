# Bid delivery modes: frozen specification (work package S0)

Status: frozen 2026-11-16 on branch `delivery`. This is the contract between the work packages C1 to D1. Background,
measurements and the reasons are in [`borrow-topup.md`](borrow-topup.md) (sections 13, 16, 17); the reference prototypes are
`contracts/prototypes/borrow/` and their engine experiments `crates/kob-tests/tests/design/borrow_topup_experiments.rs`.

**Change control.** A package that finds a rule here impossible or unsafe stops and reports; it does not improvise. Only the
integrator edits this file. Labels: **[engine]** = shown by the reference experiments; **[spec]** = decided here, to be built
and tested by the named package.

## 1. Delivery modes

A delivering order (one that hands bought tokens to its maker) carries a mode, set by the maker at placement and immutable:

| Mode | Name | Delivery |
|---|---|---|
| 0 | new | every fill creates a new plain token UTXO of `n` for the maker (today's behaviour) |
| 1 | accumulate | the order keeps what it buys in its own **bought-custody**; the maker gets one UTXO when the order ends, is cancelled or is claimed |
| 2 | top-up | the filler may add the tokens to the maker's existing **receive UTXO** and keeps `topupTip`; a new delivery stays allowed |

Definitions. *Plain*: owner = maker's key, owner scheme `0x00`, borrow scheme `0x00`, zero guard. *Bought-custody*: owner = the
order's covenant id, owner scheme `0x04`, borrow scheme `0x00`, zero guard, amount exactly `bought`. *Receive UTXO*: owner =
maker's key, owner scheme `0x00`, borrow scheme `0x01`, guard = threshold as 8 bytes little-endian followed by 24 zero bytes.
All three carry the order's extension commitment of the bought token and sit on the bought token's pinned template.

Modes 1 and 2 exist for KCC-20 tokens only. A KRON delivery is mode 0; the KRON twins are not changed.

## 2. State layouts

Encoding (unchanged): `int` = `0x08` + 8 bytes; `byte[32]` and `pubkey` = `0x20` + 32 bytes. Offsets below are **state
coordinates** of the push opcode; the redeem script is `1 byte prefix ‖ state ‖ code`, so a redeem offset is the state offset
+ 1 and an `int`'s 8 payload bytes start one byte later. Existing fields keep their offsets in every template: the new fields
are appended at the end.

The delivery block, in this order: `mode`, `recvThreshold`, `topupTip`, `bought` (all `int`; `bought` is mutable and last).

| Template | State today | New fields (state offset) | State after | `bought` payload, redeem coordinates |
|---|---|---|---|---|
| `KobBid` | 285 | `mode` 285, `recvThreshold` 294, `topupTip` 303, `bought` 312 | 321 | `[314, 322)` [engine] |
| `KobCondBid` | 381 | 381, 390, 399, `bought` 408 | 417 | `[410, 418)` |
| `KobPair` | 447 | 447, 456, 465, `bought` 474 | 483 | `[476, 484)` |
| `KobCondPair` | 543 | 543, 552, 561, `bought` 570 | 579 | `[572, 580)` |
| `KobIfdAsk` | 594 | `exitMode` 594, `exitRecvThreshold` 603, `exitTopupTip` 612 | 621 | none (immutable) |
| `KobIfdPair` | 909 | `exitMode` 909, `exitRecvThreshold` 918, `exitTopupTip` 927 | 936 | none (immutable) |

Existing mutable windows do not move (redeem coordinates): `KobCondBid` `stopPrice` `[224, 232)`, `amountLeft` `[287, 295)`,
`armed` `[296, 304)`; `KobPair` `amountLeft` `[398, 406)`, `custody` `[407, 415)`; `KobCondPair` `stopPrice` `[416, 424)`,
`armed` `[425, 433)`, `amountLeft` `[434, 442)`, `custody` `[443, 451)`; `KobIfdAsk` `[245, 253)`, `[254, 262)`, `[263, 271)`;
`KobIfdPair` `[440, 448)`, `[449, 457)`, `[458, 466)`, `[467, 475)`.

Exit states written by the entries:

- `KobIfdAsk` → `KobCondBid` exit (417 B) = the exit state as built today (381 B: `exitState[0, 321)` with its spliced
  windows, then `parent`, `rptPrice`, `rptPre`, `rptUntil`) ‖ `exitMode` ‖ `exitRecvThreshold` ‖ `exitTopupTip` ‖ `bought = 0`.
- `KobIfdPair` → `KobCondPair` exit (579 B) = the exit state as built today (543 B) ‖ the same four fields, `bought = 0`.
- A repeating entry (its exits merge back into the entry) commits `exitMode = 0`; the entry's fill requires it when its
  repeat amount is non-zero. [spec, C2 / C3]

Unchanged states: `KobAsk` (276), `KobCondAsk`, `KobIfdBid`, every KRON twin.

Placement (`check_new_order`, KOB1 record): `bought == 0`; `mode` in {0, 1, 2}; mode 0 and 1: `recvThreshold == 0` and
`topupTip == 0`; mode 2: `recvThreshold > 0` and `0 <= topupTip <= deliveryCarrier`; mode 0 required for `tif != GTC` (wallet
rule, not a contract rule), for a KRON delivery and for repeating entries. The KOB1 record format and kind codes do not
change; only the state lengths of the table above. An in-place amend may not change `mode`, `recvThreshold`, `topupTip` or
`bought`.

## 3. Entry signatures

| Template | Entries (changed ones in bold) |
|---|---|
| `KobBid` | **`fill(byte[8] nb, int tokIn, int t, int form)`**, `cancel(sig)`, `refund()` |
| `KobCondBid` | **`settle(byte[8] nb, int tokIn, int leg, int ev, int t, int form)`**, `update(int ev, int tk)`, `cancel(sig)`, `refund()` |
| `KobPair` | **`settle(byte[8] nb, int custIn, int tTplIn, int t, int sOut, int tOut, int form)`**, `cancel(sig)` |
| `KobCondPair` | **`settle(...)` as today with a last argument `int form`**, `cancel(sig)` |
| `KobIfdAsk`, `KobIfdPair` | unchanged signatures |

- `nb` stays the first push, fixed 8 bytes (trigger evidence reads it at signature-script bytes `[1, 9)`).
- `tokIn` (`tTplIn` for pairs) is the token input the delivery is built from: mode 1 with `bought > 0` → the bought-custody;
  mode 2 form 2 → the receive UTXO; otherwise any input of the bought token (only its template bytes are read).
- `form` is read in mode 2 only: 0 plain new, 1 new receive UTXO, 2 top-up. Any other value in mode 2 is refused.
- No `claim` entry. A claim is the `cancel` entry (section 7).
- `refund()` takes no argument (H1): the custody is the token's only input, `OpCovInputIdx(tokenCovId, 0)`. An
  argument could be pushed as 8 bytes, so a refund would start like a fill and pass as trigger evidence or as a merge.

## 4. Fill rules

Common to every delivering fill. `D` is the delivery slot: output `i` (the order's input index). `n > 0` is the filled amount.

| | Mode 0 | Mode 1 | Mode 2 form 0 | Mode 2 form 1 | Mode 2 form 2 |
|---|---|---|---|---|---|
| Token input checked | template hash | `bought == 0`: template hash. `bought > 0`: P2SH + exact bought-custody state | template hash | template hash | P2SH + receive state: owner = maker, schemes `0x00` / `0x01`, `int(guard[0..8]) >= recvThreshold`, `guard[8..32) == 0`, the order's commitment, amount `have >= 0` |
| Output `D` while the order continues | plain, `n` | bought-custody, `bought + n` | plain, `n` | receive UTXO, `n`, threshold `recvThreshold` | the receive UTXO's state bytes `[9, 112)` unchanged, amount `have + n` |
| Output `D` when the order ends | plain, `n` | plain, `bought + n` | plain, `n` | receive UTXO, `n` | as above |
| Continuation state | unchanged | `bought` ← `bought + n` | unchanged | unchanged | unchanged |
| KAS leaving the escrow beyond the price (`released`) | `deliveryCarrier` | `deliveryCarrier` if `bought == 0`, else 0 | `deliveryCarrier` | `deliveryCarrier` | `topupTip` |
| KAS on `D`, at least | `deliveryCarrier` + rounding | carrier (first fill) or the custody's KAS (later) + rounding | as mode 0 | as mode 0 | the receive UTXO's KAS + rounding |
| Carrier reserved per remaining fill (`canContinue`) | `deliveryCarrier` | `deliveryCarrier` if `bought == 0`, else 0 | `deliveryCarrier` | `deliveryCarrier` | `deliveryCarrier` |

Further rules:

1. `0 <= mode <= 2`. Modes 0 and 2 require `bought == 0`; mode 1 requires `bought >= 0`.
2. Mode 2, forms 1 and 2: `recvThreshold > 0`, `0 <= topupTip <= deliveryCarrier`.
3. The continuation is the order's own script with only its mutable windows spliced, exactly one output of its covenant id,
   holding exactly `left - released` (bid kinds) or what the kind requires today.
4. When the order ends: no output of its covenant id; the unspent KAS `rem = inValue - allIn - released` goes to the maker
   through the existing refund slot when `rem >= REFUND_MIN`, otherwise it rides on `D`; `rem + (KAS put on D) >= 0`.
5. Everything not named here (price, rounding, minimum fill, TIF, intervals, auctions, evidence, positional payouts) is unchanged.
6. `KobCondBid`'s booked-exit merge path (negative first push) is unchanged and requires `bought == 0`.

[engine] for `KobBid`: experiments A, B, D, F, G, H. [spec] for the other templates.

## 5. Stray guard and the order's end

- **Stray guard.** A fill, update or refund refuses every input of a token of the order that is owned by the order's covenant
  id, except: the sell custody the kind already authenticates (ask-side and pair kinds), and the bought-custody named by the
  fill when `bought > 0` and authenticated as in section 4. The scan stays unrolled over `MAX_TOK_IN` slots and reads the owner
  at the fixed distance from the end of the signature script (`KobAsk.noStrays` form with one or two excluded indices).
  A pair order scans S and T separately, each with its own excluded custody.
- **The bought-custody is known by its exact state** (amount `== bought`), so a look-alike or a second UTXO of the order's id
  never stands in for it. [engine G2, G4b, G5, G10]
- **No bought-custody is left behind.** Every path that ends the order without the maker's signature releases the whole
  bought-custody, with at least its KAS, to the maker's key as one plain UTXO:
  - ending fill: at `D`, amount `bought + n`;
  - `refund()` of bid kinds: with `bought > 0` the bought-custody is the only input of the token and its release sits at
    the custody's own input index; with `bought == 0` no input of the token, as today;
  - pair kinds' refund / zero-fill and IOC end: the bought-custody's release sits at its own input index, next to the existing
    return of the sell custody.
  [engine G8, G11 to G13 for `KobBid`]

## 6. Positional and ordering rules

1. Delivery of the order at input `i` is output `i`. A terminal bid's refund is output `M - 1 - i`; refund-carrying terminal
   bids are the first inputs (unchanged).
2. A bought-custody may sit at any token input position; it is authorised by its order's input (leader or delegate).
3. A top-up (mode 2 form 2): the receive UTXO is the token's **first** input (the leader, borrow path, witness `0x01`), and the
   topped-up order is the token's **lowest-index delivering order**, so that its `D` is the leader's first output. At most one
   top-up per token per transaction. [engine A3, A5, H2]

   **Successor binding (DLV-07).** A form-2 fill requires `OpAuthOutputIdx(tokIn, 0) == i`: its `D` is the output the
   token program's borrow path checks as the receive UTXO's successor (`next_states[0]` at the leader's first authorised
   output). Two orders naming one receive UTXO would need the same output with different amounts, so the second fails;
   a delegate `tokIn` has no authorised output and fails too. One check of a few bytes and no new layout rule: the
   builders already put `D` there. In `KobBid`, `KobCondBid`, `KobPair`, `KobCondPair` (so also the exits of
   `KobIfdAsk`, `KobIfdPair`); the router never tops up (section 12).
4. If a token has a refund-carrying terminal bid in the transaction, only that bid may be the top-up; otherwise the builder
   uses form 1 for that token.
5. Token slots per token per transaction: mode 0 and forms 0 / 1 = 0 inputs, 1 output; mode 1 = 1 input once the custody
   exists, 1 output; top-up = 1 input, 1 output. On a 3 / 3 program: two mode-1 orders, or one top-up and one mode-1 order,
   against one ask. [engine F, H2]
6. A fill of `n <= threshold` of the receive UTXO cannot top up (the token program requires a gain above the threshold); the
   builder uses form 1.

## 7. Claim through `cancel`: the shapes the pre-sign check accepts

All are signed by the maker with SIGHASH_ALL through the order's `cancel(sig)` entry. [engine F, I for `KobBid`]

| Shape | Inputs | Outputs |
|---|---|---|
| CL1 claim | order (`cancel`), its bought-custody, maker funding | the order's continuation; one plain token UTXO of `bought` to the maker; KAS change |
| CL2 claim and merge | CL1 + up to `slots.inputs - 1` plain token UTXOs of the maker | continuation; one plain token UTXO of `bought + Σ wallet inputs`; change |
| CL3 claim and place | order, bought-custody, optional wallet token UTXOs, funding | continuation **or** the escrow back to the maker; a new order (placement record, one-output genesis) with its custody; optional plain token change; KAS change |
| CL4 cancel | order, bought-custody (and the sell custody of a pair order), funding | escrow and every custody to the maker |

What the check must verify, besides what it does for an amend and a cancel-replace today:

- the bought-custody input is the P2SH of the bought token's template with amount `== bought` of the order's decoded state;
- a continuation is bound to the order's covenant id by the order's input, is the same template, and its state differs from
  the order's only in `bought = 0`; it holds at least the order's KAS plus one `deliveryCarrier` (the next custody's carrier);
- every output of the bought token is either plain and the maker's, or the custody of the order placed in the same
  transaction; their amounts sum to `bought + Σ wallet inputs`;
- wallet token inputs are plain and the maker's (receive-state inputs are not used in these shapes);
- a pair order's sell custody is not an input of CL1 to CL3;
- no other output is bound to the order's covenant id.

## 8. Router (frozen shape)

**Finding.** An Argent entry observes a fixed set of token inputs and outputs: the generated `sell` requires
`OpCovInputCount(token) == 1` and `OpCovOutputCount(token) == 2` (`contracts/argent/router/sil/TokenToKas_sell.sil:564-565`
[source]), and the shipped router refuses a transaction with a second input of the token
(`argent_router_tests::router_entry_refuses_a_token_input_it_does_not_observe` [engine]). That is also why the `_out` twins
exist today. **A bid custody cannot be an optional input: every shape with a custody input is its own entry.**

Frozen router shape [spec, C4]:

- Every existing entry with a bid leg computes that leg's pinned delivery from the bid's state: mode 1 → bought-custody of
  `bid.bought + n` owned by the bid's id; modes 0 and 2 → plain `n` to the bid's maker (form 0). With `bid.bought == 0` no
  extra input is needed, so the existing entries serve mode 0, mode 2 and the **first** fill of a mode-1 bid.
- New entries for bid legs whose bid already has a custody. The keeper orders the bid legs so that custody-bearing bids come
  first. `TokenToKas`: `sell_c`, `sell_c_out`, `sell2_c`, `sell2_c_out`, `sell2_cc`, `sell2_cc_out` (`c` = that many leading
  legs carry a custody). `TokenSwap`: one `_c` twin of each existing shape (its bid leg carries a custody). No `sell3` custody
  variants. The KRON routers are unchanged.
- A custody leg observes the custody as a token input and requires: owner = the bid's id, owner scheme `0x04`, borrow
  disabled, amount `== bid.bought`, the bid's commitment (the checks `ask_fits` makes for an ask's escrow).
- Slots on a 3 / 3 program: the lock plus at most two bid custodies; deliveries plus the payer's change at most three outputs.
- The payer's and merchant's bounds (P13) are unchanged.

## 9. Budgets

- Script sizes: `KobBid` 1,629 → about 2,680 B [engine H]. Ceiling for every other changed template: today's size + 1,150 B;
  a package that exceeds it reports before merging.
- Static sig-op bound of every order script stays at most 15.
- Compute budgets (`data/compute_budgets.json`) and keeper tips are regenerated with `KOB_REGEN=1` after the re-pin (P1, B2).
  New leader role: `{program}.leader.borrow.i{n}.o{m}`; delegates and custodies use the existing `covid` roles.
- New generated table `crates/kob-protocol/data/topup_extra.json`: per token program, the relay-fee difference at the floor
  rate between a top-up fill and a new-delivery fill of the same order (B2 measures it with the final templates). Reference
  values from the prototype: `KCC20Ref` 595,400, `KCC20PublicMint` 818,600, `KForgeToken7214` 1,455,200,
  `KaspaRocketToken11458` 2,304,000 sompi.
- `refundTip` defaults are re-measured: the refund of a mode-1 order with `bought > 0` reveals the token program.

## 10. Wallet rules (web app and CLI)

- **Mode default.** 1 for GTC orders that deliver a KCC-20 token (limit, ladder, DCA, stop-limit buys, pair orders buying a
  KCC-20 token, the exits of non-repeating entries); 0 for IOC / FOK / market orders, KRON deliveries and repeating entries;
  2 only when the user switches it on in the advanced settings.
- **Tip.** `topupTip = min(deliveryCarrier, ceil(1.25 × topup_extra[program]))`. The user may raise it up to `deliveryCarrier`.
- **Threshold.** `RECV_THRESHOLD_SOMPI = 500,000,000`. At the wallet's first mode-2 order for a (token, extension commitment):
  `T = max(1, ceil(RECV_THRESHOLD_SOMPI × scale / price))`, `price` = the order's limit price in sompi per whole token (pair
  orders: the KAS-book price used for the default pair minimum fill; without one, mode 2 is not offered). `T` is stored per
  (wallet key, token covenant id, extension commitment) and reused; it changes only when the user resets it. An order's
  `recvThreshold = min(T, minFill - 1)`; if that is 0, mode 2 is not offered for the order.
- **Funding.** A mode-1 order is funded with one delivery carrier; modes 0 and 2 as today (one per possible fill).
- **Display.** Tokens in a bought-custody count as the maker's, shown as "bought, held by the order", with Claim, Claim and
  sell, Claim and merge. Tokens in receive UTXOs count as free balance.
- Receive-state holdings are spent through option A (already on master before B2, W1, L1 start).

## 11. Work packages: base and file ownership

Base of every package: branch `delivery` at the commit that adds this file. Each package works in its own worktree and
branch (`delivery-<wp>`), edits **only the files it owns**, adds its own `[[test]]` entry to `crates/kob-tests/Cargo.toml`
(the integrator resolves that file), and is merged into `delivery` by the integrator in the order of the "after" column.

| WP | Owns | After |
|---|---|---|
| C1 `KobBid` | `contracts/orders/KobBid.sil`, `KobBid.ctor.json`; new `crates/kob-tests/tests/kob/kob_bid_modes_tests.rs`; `tools/ablation/catalogs/` entries for it | S0 |
| C2 `KobCondBid`, `KobIfdAsk` | `contracts/orders/KobCondBid.sil`, `KobIfdAsk.sil`, their ctor files; `crates/kob-tests/tests/kob/kob_v2_buy_tests.rs` | S0 |
| C3 pairs | `contracts/orders/KobPair.sil`, `KobCondPair.sil`, `KobIfdPair.sil`, ctor files; `crates/kob-tests/tests/common/pair_harness.rs`, `tests/kob/kob_pair_tests.rs`, `kob_pair_shapes_tests.rs`, `kob_pair_custody_ext_tests.rs`, `kob_cond_pair_tests.rs`, `kob_ifd_pair_tests.rs`, `tests/audit/audit_pair_sec_tests.rs` | S0 |
| C4 router | `contracts/argent/kob_router.ag`, `contracts/argent/KOBOrders*`, `contracts/argent/router/`, `tools/router-gen/`; `crates/kob-tests/tests/argent/argent_router_tests.rs`, `tests/audit/c4_security_tests.rs` | S0; final handle after C1 |
| P1 re-pin | `scripts/build-contracts.sh`, `build-argent.sh`, `extract-retired.mjs`; `contracts/artifacts/`, `contracts/SHA256SUMS`; `crates/kob-protocol/src/artifacts.rs`, `retired.rs`, `data/retired/`; `crates/kob-tests/tests/kob/retired_cancel_tests.rs`, `artifacts_tests.rs`, `registry_tests.rs`; `docs/spec/template-retirement.md`; `KobCondAsk` / `KobIfdBid` re-pin | C1 to C4 |
| B1 codec | `crates/kob-protocol/src/state.rs`, `state/`, `payload.rs`; `crates/kob-wasm/src/lib.rs`; `web/src/kob/types.ts`; `docs/spec/kob1-payload.md` | S0 |
| B2 builder | `crates/kob-protocol/src/tx.rs`, `script.rs`, `build.rs`, `budget.rs`, `data/compute_budgets.json`, `keeper_tips.json`, `topup_extra.json`; `tests/builders.rs`, `bid_refund.rs`, `budget_table.rs`, `cost_table.rs`, `carrier_fees.rs`, `golden.rs`, vectors | P1, B1 |
| B3 pair builder | `crates/kob-protocol/src/build/pair.rs`, `data/pair_keeper_tips.json`, `tests/pair_orders.rs` | P1, B1 |
| B4 router builder | `crates/kob-protocol/src/build/intent.rs`, `router.rs`, `data/router_sil_abi.json`, `tests/intent_builders.rs`, `route_families.rs` | P1, B1 |
| B5 claim plans | `crates/kob-protocol/src/build.rs` (cancel path) and `tx.rs` (`SigPlan` kinds) **after B2 is merged**; `tests/amend.rs`, `sweep.rs` | B2 |
| K1 indexer | `crates/kobd/src/indexer/`, `crates/kobd/src/api/`; `tests/flow_*.rs`, `custody_exactness.rs`, `storage_migrate.rs` | B1 |
| K2 matcher | `crates/kobd/src/matcher/`; `tests/matcher_*.rs`, `c6_planner_props.rs`, `planner_fuzz.rs`, `submit_conflict_backoff.rs` | B2, B3, K1 |
| K3 keepers | `crates/kobd/src/keepers/`, `maintenance.rs`, `x402/`; `tests/keepers.rs`, `keeper_liveness.rs`, `x402_*.rs` | B2, B4, K1 |
| X1 x402 | `crates/kob-x402/`; `docs/spec/x402-swap-and-pay.md` | B4 |
| L1 CLI | `crates/kob-cli/` | B2, B3, B5 |
| W1 web placement | `web/src/kob/orders/`, `web/src/kob/programs.generated.ts`, ticket and settings UI | B1 |
| W2 web holdings and claim | `web/src/kob/balances.ts`, `cancel.ts`, `merge.ts`, `positions.ts`, `web/src/ui/orders/`, `web/src/data/token-tracker.ts` | B5, K1 |
| W3 pre-sign and signer | `web/src/kob/decode.ts`, `plan-types.ts`, `plan.ts`, `web/src/signer/` | B5 |
| D1 docs | `docs/spec/order-types.md`, `matcher.md`, `docs/ops/executor.md`, `docs/api-reference.md`, `docs/user-guide.md`, `docs/security/AUDIT-2026-10.md` | S0 |

Can start now, in parallel: C1, C2, C3, C4 (to the handle), B1, D1. Never together: B2 and B5 (`build.rs`, `tx.rs`).
The prototypes and `tests/design/` are reference only: no package edits them, and they are removed at the re-pin (P1).

## 12. Known limitations

- **No top-up through the router or an x402 swap.** The router delivers a mode-2 bid leg as form 0, plain `n` to the
  maker (section 8), and the x402 swap verifier refuses a borrow-enabled token input, so a receive UTXO is never topped up on
  those paths. A top-up is a matcher fill only (section 6); the mode-2 order itself stays fillable through both.
- **Router custody shapes with a refunding plain bid.** `TokenToKas_sell2_c_out` and `TokenSwap_swap2_out_c` cannot take a
  bid with a bought-custody whose end carries no refund followed by a plain bid whose end carries one.
