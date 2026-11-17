# Red-team coverage: kaspa-redteam patterns and KOB's tests

Kaspero Labs' red team ([REDTEAM-2026-11-kaspero.md](REDTEAM-2026-11-kaspero.md), kaspa-redteam 0.17.2, library 0.10.2) is
the reference. This page maps each of its attack patterns, and the delivery-mode patterns added for
[bid-delivery-spec.md](../design/bid-delivery-spec.md), to the tests KOB already had and to the red-team suites added on
branch `delivery-rt`. A pass is not an audit or a guarantee.

## Where the tests live

| Layer | Tests | What they do |
|---|---|---|
| Hand-built scenarios | `crates/kob-tests/tests/kob/*` (`kob_bid_modes_tests`: `PM*`/`N*` delivery scenarios, `kob_v2_*`, pair suites), `tests/argent/argent_router_tests.rs`, `tests/audit/*` | named attacks (`run_bad`) and positives on a few programs |
| Ablation | `tools/ablation/catalogs/*` | removes one `require`, checks the named attacks flip |
| Builder hardening | `crates/kob-protocol/tests/builder_hardening.rs`, `build::build_batch_unchecked` (`adversarial`) | the builders refuse hostile requests |
| Pre-sign | `web/src/kob/decode-*.test.ts` (`decode-claim`, `decode-mutation`, `decode-hardening`), `web/src/signer/*` | the wallet refuses hostile plans |
| Fuzz | `crates/kobd/tests/hostile_fuzz.rs`, `planner_fuzz.rs`, `matcher_fuzz.rs` | hostile states and plans through the matcher and keepers |
| **Red team (new)** | `crates/kob-tests/tests/redteam/` (`redteam_library_tests`, `redteam_delivery_tests`) | one mutation per pattern on **every builder shape** (the tx dump's, plus claims, IOC / FOK ends with a custody, the pair delivery grid, the router intents) on **every token program**, from the taker, matcher and keeper seats |
| Dump for external tools | `crates/kob-protocol/tests/tx_dump.rs` (now with the pair delivery grid, the claims and the IOC / FOK custody ends) | `kob-tx-dump/1` for kaspa-redteam baselines |

The red-team suites are an add-on: they build their shapes with the existing fixtures (`kob-protocol/tests/common`) and the
production builders, and judge them in the same engine. They exist as a separate directory because none of the layers above
mutates the engine-built transactions of every shape generically; the hand-built suites name one transaction per attack.

## Coverage per attack id

Counts: hostile transactions judged by the new suites (all programs; `refused` / designed `notes` / `findings`). The pair
tests are `#[ignore]` (heavy); the counts include them.

| Id | Pattern | Existing tests (before) | New: judged / refused / notes / findings |
|---|---|---|---|
| OUT-01 | redirect an output (KAS: the script; token: the owner) | kaspa-redteam runs; `intent_builders` redirect negatives; suite payout negatives | 36,053 / 35,990 / 63 / 0 |
| OUT-02 | output 0 cut to 10% | kaspa-redteam runs | 6,653 / 6,653 / 0 / 0 |
| OUT-03 | a later output halved | kaspa-redteam runs; `NC07`, `NC12`, `NT06`, `NR03` | 22,747 / 22,490 / 257 / 0 |
| OUT-04 | keep the leftover | kaspa-redteam runs; keeper tip tables | 6,182 / 60 / 6,122 / 0 |
| FEE-01 | every output halved | kaspa-redteam runs | 6,653 / 6,653 / 0 / 0 |
| IN-01 | a second copy of the covenant input | kaspa-redteam runs | 13,306 / 13,306 / 0 / 0 |
| IN-02 | attacker coin at input 0 | kaspa-redteam runs | 6,653 / 6,653 / 0 / 0 |
| SIG-01 / SIG-02 | invalid / attacker signature on maker-signed paths | `NCL1`..`NCL4`, cancel-by-another-key negatives, `retired_cancel_tests` | 742 + 742 / all refused |
| FAM-10 | stray of every token, owned by every covenant input, swept (all builds, KRON included) | `NG01`, `NG06`, `NR08`, `NR11`, `NR12`; ablation `AU-*-S<k>`; kaspa-redteam on `KCC20Ref` only | 38,424 / 26,918 / 11,506 (foreign strays, by design) / 0 |
| BOR-01 | custody with borrowing on (every KCC-20 build) | `NC13`, `NC15`, `NR05`; kaspa-redteam on `KCC20Ref` only | 13,362 / 13,362 / 0 / 0 |
| TOK-01 | half a token output to the attacker | `NC01`, `NC10`, `NT01`, `NR07` | 17,626 / 17,602 / 24 / 0 |
| DLV-01 | custody stand-in (another order's, own UTXO, +/-1, borrow, extension, scheme, swapped custodies) | `NC02`, `NC05`, `NC15`, `NR06`, `NR12`; router `X1`..`X5` | 9,420 / 9,420 / 0 / 0 |
| DLV-02 | look-alike receive state | `NT02`..`NT05`, `NT08`, `NT10`, `NF05`, `NF06` | 6,741 / 6,741 / 0 / 0 |
| DLV-03 | cross-mode delivery, mode or `form` changed | `NM01`..`NM07b`, `NF01`..`NF04`, `NS01`..`NS08`, `PM0b`, `PM1a` | 94,986 / 51,786 / 43,200 / 0 |
| DLV-04 | tip over-take | `NM07`, `NM07b`, `NT06`, `NT11`, `NC07`, `NC08`, `NC12`, `NC18` | 31,603 / 31,603 / 0 / 0 |
| DLV-05 | claim by a non-maker; refund before due | `NCL1`..`NCL4`, `NR10`; `web/src/kob/decode-claim.test.ts` | 234 / 234 / 0 / 0 |
| DLV-06 | `bought` inflated / deflated | `NC01`, `NC06`, `NC17`, `NS03`..`NS05` | 1,562 / 1,562 / 0 / 0 |
| DLV-07 | a second top-up of one receive UTXO | `NO04` (two borrowed inputs: the token refuses); since the fix `NO07`, `DM2-36`, pair `NM20` (ablation `DM-31`, `DM-CB-topup-succ`, `DM-P10`, `DM-CP2`) | 9 / 9 / 0 / 0 (fixed) |
| DLV-08 | the receive successor off its slot | `NO01`, `NO03`, `PO05` | 963 / 963 / 0 / 0 |
| DLV-09 | stray-guard bypass with an exact look-alike custody | `NG01`, `NC04`, `NR08`, `NR12` | 2,718 / 2,718 / 0 / 0 |
| DLV-10 | fill or claim keeps `bought` while the custody leaves | `NC03`, `NC09`, `NC11`, `PCL1` | 238 / 238 / 0 / 0 |
| DLV-11 | refund drops / redirects / skims / moves the custody | `NR01`..`NR05`, `NR13` | 90 / 90 / 0 / 0 |
| DLV-12 | ending fill leaves the custody behind | `NC11`, `NC16` | 769 / 769 / 0 / 0 |

The full attack x template x program matrix is printed by each test (`RT|attack|template|program|judged|refused|notes|findings|dropped|by_target`),
and written as JSON per test when `KOB_RT_OUT` is set.

## Designed notes (the allow-list, `rt.rs` `ALLOW` and the attacks' own rules)

- Leftover within a field the covenant names as a tip, fee or budget (`refundTip`, `keeperTip`, `topupTip`, `max_extra`), or
  no more than what the spend already pays the seat (kaspa-redteam's rules).
- The signer's own payment: a plain output, or a key-owned token output, to a key no covenant input names, funded by the seats.
- `router-carrier`: a router execution pins the token amount, owner and extension of the cells it delivers, not their KAS carrier
  or the payer's filler output (report section 3, maintainer's answers).
- `foreign-stray`: a token of another covenant id owned by an order's id is not protected ([matcher.md](../spec/matcher.md) 1.2).
- `signed-by-maker`: a maker-signed `cancel` authorises any outputs (claims, amends).
- DLV-03: `form` is read in mode 2 only (spec section 3); a mode change that leaves the spec's delivery the same (plain `n`);
  a refund, which releases the escrow and the custody to the maker whatever the mode.

## DLV-07 (fixed)

Two mode-2 bids of one maker are filled in one transaction. Bid 0 tops up the maker's receive UTXO R (form 2, R leads its token
on the borrow path). Bid 1 also claimed form 2 with R as its `tokIn` and delivered a second token output with R's state and
`have + n` units, not R's successor. The token program constrains only R's successor, so before the fix every input and
consensus accepted it: the maker paid `topupTip` for a delivery that topped nothing up, the filler `have` extra units.

Fix (spec section 6.3, successor binding): a form-2 fill of `KobBid`, `KobCondBid`, `KobPair` and `KobCondPair` requires
`OpAuthOutputIdx(tokIn, 0) == i`, so its delivery is the output the borrow path checks; a second order naming R fails. The
red-team test `delivery_second_topup` now requires every attempt refused by an order (9 programs).

## What is not covered

- Mempool policy (fee floor, standardness), lock-time finality and sequence locks are not re-applied by the native judge (no
  attack changes them except DLV-05's earlier lock time).
- `IN-02` and part of `FEE-01` / `OUT-02` are mostly refused by transaction rules (mass, zero outputs), as in kaspa-redteam.
- The DLV attacks are generic over the shapes; patterns that need a hand-made transaction no builder lays out (two receive
  UTXOs on the borrow path) stay with `kob_bid_modes_tests`.
- KRON programs have no borrow setting: BOR-01 and DLV-02 do not apply; modes 1 / 2 are KCC-20 only.
