# Ablation results

## Pre-mainnet covenant fixes (2026-10-12): every suite of the changed templates, in full

Contracts of `8f5784f`: the IFD merge scale (KobIfdBid / KobIfdAsk and the KRON twins), the exit re-arm bound n < 2^53
(KobCondAsk / KobCondBid and the KRON twins) and the authenticated stop-trigger evidence custody (tplState on `tk`: every
conditional and if-done kind of both families, KobCondPair / KobIfdPair `rd` and `evLeg`). Every suite whose contracts
changed was run in full on the final test sources (debug executables, `--exe-dir`, one suite per invocation, baselines
clean in every run). The KCC-20 touch entries and the pair suites now also run on the reference 3/3 program (`KCC20Ref`,
tradable on mainnet), besides 8/8; a scenario whose shape exceeds 3 token inputs / outputs is skipped there.
**698/698 confirmed, 30/30 defence-in-depth held**, no `NOT-CONFIRMED`, `EDIT-ERROR` or `BASELINE-DIRTY`:

| Suite | Entries | Confirmed | Held | Wall time (jobs) |
|---|---|---|---|---|
| `kcc20-sell` | 101 | 98/98 | 3 | 991 s (4) |
| `kcc20-buy` | 101 | 97/97 | 4 | 1,950 s (3) |
| `kron-sell` | 148 | 145/145 | 3 | 1,127 s (4) |
| `kron-buy` | 114 | 110/110 | 4 | 2,424 s (3), IA24 re-run 270 s |
| `cond-pair` | 126 | 116/116 | 10 | 78 s (3) |
| `cond-pair-rpt` | 12 | 10/10 | 2 | 46 s (3) |
| `ifd-pair` | 126 | 122/122 | 4 | 317 s (3) |
| `pair` | 84 | not re-run (KobPair unchanged) | | |

* **New checks** (each removed alone flips the attack only it refuses): `tktpl` / `tkp2sh` per touch function (`TA`, `TU`,
  `TV`, `TK` of both families; cond-pair `EVTKTPL`, `EVTKP2SH`, `EVBTKTPL`, `EVBTKP2SH`; ifd-pair `IF116`..`IF119`): the
  custody stand-ins 02c (a look-alike holder of the token's covenant id under another program) and 02d (the genuine holder
  redeem pushed by a non-P2SH input), `NC5H*` / `NC5S*` / `NC3H*` / `NC3S*`, `NE25*` / `NE26*`; they flip at the order's input
  only (`inputOnly`: the evidence ask's own stray guard, which reads the owner at the same offset, refuses such an input too).
  Merge scale `V-IB-scale` (NRP30), `V3-I-scale`, `IA-merge-scale` (NRPB36, both catalogs): every input accepts without it.
  Re-arm bound `V-CA-mergek` (V3CA09, every input), `V3-C-mergek` (input-only: the KRON program refuses the oversized
  custody), `CB-rearm-mergek` (NV3CB43, both families), cond-pair-rpt `CR12` (NCR10, every input; formerly listed as
  unreachable).
* **Repaired entries** (no longer applied or no longer witnessed by their scenario before this run, independent of the
  contract change unless noted): `B8` / `R9` / `D9` / `IA13` / `IF34` repointed to dating the booked exit's `rptUntil` by
  the filler's `t` (the `int utxoDaa` they edited is gone); `CB7` repointed to the keeper's `in - keeperTip` floor with a new
  witness `NB30c` (`NB30`, `NB25` held: the budget floor also refuses them); kron-buy `IA24`: its scenario `NRPB2` broke the
  re-arm rule of the fill first (rptAmount 4 whole tokens), now rptAmount 4 base units as in the KCC-20 suite. ifd-pair:
  the pending entries of the pair phase all confirm: `IF88`, `IF89`, `IF95`, `IF96`, `IF97` `inputOnly` (the edited evidence
  order's own script refuses its doctored token / scale / slope), `IF90` / `IF98` (custody of the other token: with the
  template check the mixed pairs are a hold; `NE18k*` on KCC-20 / KCC-20 isolate the covenant-id check), `IF104c` (`NIM9`
  rebuilt: with the exit at input 0 the merge of 0 encoded as negative zero, the entry's refund), `IF107` (`NIX3` on
  KCC-20 / KCC-20; the mixed-pair `NIX3m` is held by `IF107` and the new `IF107x`, flipped by `IF107c`).
* The detail tables of these seven suites below are this run's (`pair` keeps its 2026-10-06 table).

## Refund rule (2026-10-06): the refund of an empty sell-first entry

`IA-refund-held` in `kcc20-buy` (KobIfdAsk) and `kron-buy` (KobIfdAskKron): the refund (`settle` nb = 0) requires
`amountLeft > 0`. Debug executables, `--jobs 2`, baselines clean. **2/2 confirmed**: without the check the refund of an
empty repeating entry with a zero-amount token UTXO as its custody (`SR1b`, and `SR1c` paying the maker) is accepted by every
input of the KCC-20 suite; on both KRON templates the entry accepts `SR2` while the KRON token program rejects the
zero-amount custody on its own (`inputOnly`).

## Pair phase (2026-10-06): KobPair, KobCondPair, KobIfdPair

The pair suites (`crates/kob-tests/tests/kob_pair_tests.rs`, `kob_cond_pair_tests.rs`, `kob_ifd_pair_tests.rs`, harness
`common/pair_harness.rs`) replace the cross limit suite; the cross limit catalogs are gone with `KobCross`. Release
executables, `--jobs 2`, one suite per invocation, baselines clean in every run. Contracts: the pair templates after
`c0b7a3e` (KobPair requires both scales positive).

| Suite | Contract | Entries | Confirmed | Held | Not run |
|---|---|---|---|---|---|
| `pair` | KobPair | 84 | 82/82 | 2 (T5, N2) | 0 |
| `cond-pair` | KobCondPair (all but the repeat-IFD checks) | 122 | 112/112 | 10 | 0 |
| `ifd-pair` | KobIfdPair | 120 | 63 in earlier runs (IF1..IF22: 21; IF23..IF60, IF62..IF65: 42) | 2 (IF9, IF11) | 78 in their current form (below) |
| `cond-pair-rpt` | KobCondPair repeat / merge checks | 11 | 2 (CR1, CR2) | 0 | 9 |

* **pair / cond-pair complete**: every `require` of KobPair and KobCondPair has an entry (expect, or hold with the
  scenarios that reach it plus a combined entry), except the documented unreachable ones in each catalog's comment block
  (`sb.length == 65` in cancel, the refund's `refundTip >= 0`); the KobCondPair repeat / merge requires are in
  `cond-pair-rpt`. KobPair's fake-quote checks (custody == amountLeft, the bid's exact quote, the order funding its own
  delivery carrier and tip) also flip the conditional armed by such a quote in evidence mode 1
  (`kob_cond_pair_tests::cond_pair_evidence_fake_quotes`, NC40..NC43): the harness recompiles KobCondPair against the
  mutated KobPair.
* **ifd-pair / cond-pair-rpt incomplete (pending for the next security session)**: all 131 entries apply (`--dry-run`),
  and the suite is green, but 87 entries (78 + 9) were never run in their current form: ifd-pair IF4 (new expect), IF8b, IF8c, IF47 (re-split into
  holds + a combined entry after IF8b failed to confirm: NI23 / NIO10 are also refused by IF47), IF61 (scenario NIU8
  rebuilt after it read index -1), IF66..IF76, IF77s0..s7, IF85..IF115 (incl. IF104c, IF110, IF110c); cond-pair-rpt CR3,
  CR4, CR6..CR11, CR9c. Expect `inputOnly` on IF70r, IF75, IF86, IF93, IF109, CR6, CR8 (a token program or the other
  order refuses too). Unreachable (catalog comments): KobIfdPair fill-branch `n > 0` (entered only with n > 0), cancel
  `sb.length == 65`, KobCondPair re-arm `n < MERGE_K` (only an attacker-funded look-alike exit of 2^53 units reaches it).
* **Finding NP64 (fixed)**: KobPair had no `scale > 0` check; a maker's own ask with a negative scale of A filled for one
  base unit of B. Self-harm only (only the maker sets its terms; builders refuse them). Fixed in `c0b7a3e`
  (`require(sScale > 0); require(tScale > 0)`), attacks NP64a / NP64b, mutations C9s / C9t confirmed.
* **c6 fuzzer, first pair long run** (release, one thread, seed 2026100601, 5,400 s): 3,106 seeds (515 pair shapes),
  3,621,447 mutants, 1,026,910 accepted, 12 findings, all `orderState` mutants of a pair order's own terms: 7 KobPair
  states with a negative scale of A (NP64, now refused by the contract; the oracle also leaves such makers unvalued,
  `INVALID_TERMS`) and 5 sell-first KobIfdPair stop entries with a limit of 0 or below (an oracle bug: the covenant's
  `p > 0` makes the seller's worst quote 1, not "no fill"; fixed). `c6_pair_long_run_regressions` replays all 12: the
  oracle is clean. A second long run on the final contracts is pending (not run in this session).

Detail tables: [pair](#pair) (this phase), [cond-pair](#cond-pair), [cond-pair-rpt](#cond-pair-rpt) and [ifd-pair](#ifd-pair) (the 2026-10-12 run, which replaces the partial ifd-pair runs of this phase) at the end of this file.

## Protocol v3 KAS kinds (2026-10-05/06)

Protocol v3 ("no lots", contracts `8dd4ebf`, 2026-10-05): the four order suites re-mapped to the v3 contract sources and
the ported harness suites (amounts in base units, `scale`, `minFill`, `quoteOf` rounding in the maker's favour, repeat
merge argument `-(k * 2^53 + m)` with the 0x08 first-byte checks, the exit update's parent guard), plus a mutation for
every new v3 check. **424/424 confirmed, 14/14 defence-in-depth held** (438 mutations in four suites), no
`NOT-CONFIRMED`, `EDIT-ERROR` or `BASELINE-DIRTY`:

| Suite | Mutations | Confirmed | Held | v2.6 (2026-10-03) |
|---|---|---|---|---|
| `kcc20-sell` | 95 | 92/92 | 3 (B1, B2, TT-step) | 74: 71/71 + 3 |
| `kcc20-buy` | 94 | 90/90 | 4 (D1, D2, TW-step, FXL3e) | 80: 77/77 + 3 |
| `kron-sell` | 142 | 139/139 | 3 (R1, R2, TT-step) | 123: 120/120 + 3 |
| `kron-buy` | 107 | 103/103 | 4 (IA17, IA18, IA24a, TW-step) | 97: 93/93 + 4 |
| `kcc20-cross`, `kron-cross` | removed in the pair phase (KobCross retired; see the pair suites above) | - | - | 48 + 48: 46/46 + 2 each |
| total | 438 | 424/424 | 14 | 470: 453/453 + 17 |

Runs: debug executables of the final test sources (a private copy per suite, `--exe-dir`), `--jobs 2`, one suite per
invocation (2026-10-05/06): kcc20-sell 736 s (plus a 94 s re-run of the seven merge-push mutations after V3CA08 was
added: `V-CA-08` changed from held to confirmed, the rows below are that re-run's), kcc20-buy 1,165 s, kron-sell
1,430 s (plus a 9 s re-run of `V3-C-rpt` after its `inputOnly` marker was dropped: confirmed on every input),
kron-buy 2,693 s. Each run did its own baseline first (clean).

* **Cross limit catalogs**: removed in the pair phase with `KobCross` (the pair suites above replace them).
* **New v3 checks** (each removed alone flips the attack only it refuses; ids per catalog, see the tables):
  minFill (`V-A-minfill`, `V-B-minfill`, `V-B-minpos` = minFill > 0 in KobBid, `V-CA-minfill`, `V-IB-minfill`,
  `CB-minfill`, `IA-minfill`, `V3-A-minfill`, `V3-B-minfill`, `V3-B-minfill0`, `V3-C-minfill`, ...), the `quoteOf`
  ceil / floor choices (`V-A-ceil`, `V-B-floor`, `V-B-ceil`, `V-CA-ceil`, `V-CA-rb`, `V-IB-floor`, `V-IB-mceil`,
  `CB-spend-floor`, `CB-rearm-proceeds-ceil`, `IA-proceeds-ceil`, `IA-pre-ceil`, `IA-merge-prefund-ceil` and the KRON
  twins), p >= tip / legPrice >= tip / price >= tip (`V-A-tip`, `V-CA-tip`, `IA-price-tip`, `V3-A-tip`, `V3-C-tip`,
  `IA-pricetip`), FOK in base units (`V-B-fok`), the 0x08 first-byte merge checks on both sides (`V-CA-08`, `V-IB-08`,
  `CB-merge-0x08`, `IA-merge-0x08`, `V3-C-push`, `V3-I-push`, `CB-pin08`, `IA-k08`), the exit update's parent guard
  (`V-CA-parent`, `CB-update-parent`, `V3-C-parent`, `CB-updateparent`), the booking bound n < MERGE_K
  (`V-IB-mergek`, `IA-booking-mergek`, `V3-I-mergek`), the evidence scale equality and minTouch (touch keys `scale` /
  `mintouch`, formerly `unit` / `units`).
* **Defence in depth that is new in v3**: the 0x08 checks also refuse the old update-aliasing attacks (NRP22, NRP28,
  NRPB32..34), so `A3`, `B3`, `C3` and `FXL3e` hold those scenarios and new combined entries (`V-CA-08x`, `C3b`,
  `FXL3g`, `C16b`) remove both checks and flip them. On KRON the booking bound and the exit-side 0x08 attack need a
  custody above 2^53 / 2^55 base units, which a KRON token program never accepts (it refuses token outputs above 10^9
  base units), so those flips are `inputOnly` there (the attacked input's own check is still the only one refusing it).
* **Engine-level, no mutation**: the overflow scenarios (`V3A05`, `V3B07`, `NV3CB30`, `NV3IA30`, ...: the largest
  fitting full fill validates, one more base unit fails even when paid the exact value) are refused by the engine's
  checked arithmetic (`NumberTooBig`), not by a contract check, so they have no catalog entry.

## kcc20-sell

KCC-20 v2; test binary `kob_v2_tests`; contracts `contracts/v2`. 98/98 confirmed, 3/3 defence-in-depth held (expected NOT to flip).
Baseline (no mutation): v2_lifecycle_stop_auction_keeper_trailing clean, v3_rounding clean, v2_fix_pass_regressions clean, v3_merge_push clean, v2_negative_trigger_manipulation clean, v2_touch_cond_settle clean, v2_touch_cond_settle_3x3 clean, v2_repeat_ifd_attacks clean, v2_touch_cond_arm clean, v2_touch_cond_trail clean, v2_touch_cond_arm_3x3 clean, v2_touch_cond_trail_3x3 clean, v2_touch_ifd_arm clean, v2_touch_ifd_fill clean, v2_touch_ifd_fill_3x3 clean, v3_min_fill clean, v3_tip clean, v2_lifecycle_stray_custody clean, v3_fok clean, v2_touch_ifd_arm_3x3 clean, v2_lifecycle_ifd_stop_entry clean.

| id | contract | check ablated | expected | flipped | status |
|---|---|---|---|---|---|
| A1 | KobCondAsk | TP without the entry only from rptUntil | NRP10 | NRP10 | CONFIRMED |
| A2 | KobCondAsk | stop leg refuses the entry (present == 0) | NRP13 | NRP13 | CONFIRMED |
| A3 | KobCondAsk | entry's merge argument = -(own index * 2^53 + n) (NRP28: the entry's arming update starts with a minimal index push and is also refused by the 0x08 first-byte check, see V-CA-08x; V3CA07: an 8-byte negative ev in the entry's update is refused by the entry itself) | NRP14; stay rejected: NRP28, V3CA07 | NRP14 NRP19(F) | CONFIRMED |
| A4 | KobCondAsk | maker payout floor (partial re-arming TP: maker >= proceeds - budget) | NRP11 | NRP11 | CONFIRMED |
| A5 | KobCondAsk | sell-out: entry gets the budget and the exit carriers | NRP12, V3CA04 | NRP12 V3CA04 | CONFIRMED |
| B1 | KobIfdBid | exit is a genuine KobCondAsk (template + P2SH check) (the crafted input also fails the exit-terms comparison, confirmed on its own by FXL3a/b, so removing the template check alone holds) | stay rejected: NRP21 |  | held (defence in depth) |
| B2 | KobIfdBid | exit's parent = this entry (the parent check is part of the exit-tail comparison with rptPrice; a plain exit also fails on rptPrice, so removing the parent alone holds) | stay rejected: NRP20 |  | held (defence in depth) |
| B3 | KobIfdBid | exit sells exactly m (its sigscript n; V3IB07: the exit refunds, n = 0) (NRP22: the exit's update starts with a minimal index push, refused by the 0x08 first-byte check) | V3IB07; stay rejected: NRP22 | V3IB07 | CONFIRMED |
| B4 | KobIfdBid | m > 0 | NRP23 | NRP23 | CONFIRMED |
| B5 | KobIfdBid | continuation SPK equality (fill/merge) | NRP17 | NRP3 NRP3b NRP16 NRP17 NRP17b NRP18 NRP29 | CONFIRMED |
| B5b | KobIfdBid | empty stop entry re-arms unarmed | NRP18 | NRP18 | CONFIRMED |
| B6 | KobIfdBid | merge floor (the budget of m) | NRP15 | NRP15 | CONFIRMED |
| B7 | KobIfdBid | stray scan in merge (shares the fill guard) | NRP24 | NRP24 | CONFIRMED |
| B8 | KobIfdBid | booking: the exit's rptUntil counts from the entry UTXO's DAA (its last activity), never from the filler's t (the cycle dated by t instead) | NRP6 | NRP6 | CONFIRMED |
| B9 | KobIfdBid | repeating entry never terminates on a fill | NRP7 | NRP7 | CONFIRMED |
| M2 | KobCondAsk | control: stop auction time >= arming origin (require(t >= newArmed)) | NA3 | NA3 | CONFIRMED |
| M4 | KobCondAsk | control: keeperTip >= 0 on update | NK2 | NK2 | CONFIRMED |
| FXE1 | KobAsk | the IOC return is positional (tokOut == tokenIn): no shared remainder, no remainder that is also a bid delivery | FXE1, FXE2 | FXE1 FXE2 | CONFIRMED |
| FXL3a | KobIfdBid | the merge requires the exit terms [0..181) (maker, legs) to be the committed exitState | NRP25, NRP27 | NRP25 NRP27 | CONFIRMED |
| FXL3b | KobIfdBid | the merge requires parent = this entry and rptPrice = its budget rate (price + tip) | NRP26 | NRP20 NRP26 | CONFIRMED |
| V-IB-scale | KobIfdBid | merge: the committed exit (exitState, compared with the exit's state) quotes at the entry's scale (else the exit returns budget(m) at its scale and the entry re-arms m at its own) | NRP30 | NRP30 | CONFIRMED |
| TA-push08 | KobCondAsk | touchAsk (settle, stop leg): the evidence sigscript starts with the fixed 8-byte push of its fill argument (0x08) | NTA10 | NTA10 | CONFIRMED |
| TA-npos | KobCondAsk | touchAsk (settle, stop leg): the evidence fill argument n > 0 | NTA11 | NTA11 | CONFIRMED |
| TA-tpl | KobCondAsk | touchAsk (settle, stop leg): the evidence is an instance of the KobAsk / KobBid template (template hash; P2SH kept) | NTA17 | NTA17 | CONFIRMED |
| TA-p2sh | KobCondAsk | touchAsk (settle, stop leg): the evidence UTXO is the P2SH of the redeem script read (template hash kept) | NTA18 | NTA18 | CONFIRMED |
| TA-token | KobCondAsk | touchAsk (settle, stop leg): the evidence trades this token (tokenCovId) | NTA07b | NTA07b | CONFIRMED |
| TA-scale | KobCondAsk | touchAsk (settle, stop leg): the evidence quotes per the same scale (prices comparable) | NTA08 | NTA08 | CONFIRMED |
| TA-mintouch | KobCondAsk | touchAsk (settle, stop leg): the evidence fill is >= minTouch base units | NTA09 | NTA09 | CONFIRMED |
| TA-slope | KobCondAsk | touchAsk (settle, stop leg): the evidence does not decay (slope 0) | NTA05 | NTA05 | CONFIRMED |
| TA-cltv | KobCondAsk | touchAsk (settle, stop leg): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTA01, NTA02, NTA03, NTA04 | NTA01 NTA02 NTA03 NTA04 | CONFIRMED |
| TA-interval | KobCondAsk | touchAsk (settle, stop leg): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTA04 | NTA04 | CONFIRMED |
| TA-active | KobCondAsk | touchAsk (settle, stop leg): exposure counts the evidence activeFrom | NTA03 | NTA03 | CONFIRMED |
| TA-tokdaa | KobCondAsk | touchAsk (settle, stop leg): exposure counts the evidence ask custody DAA | NTA02 | NTA02 | CONFIRMED |
| TA-tkcov | KobCondAsk | touchAsk (settle, stop leg): the custody input tk carries the token covenant id | NTA02b | NTA02b | CONFIRMED |
| TA-owner | KobCondAsk | touchAsk (settle, stop leg): the custody input tk is owned by the evidence ask (owner = its covenant id, scheme covenant id) | NTA15, NTA16 | NTA15 NTA16 | CONFIRMED |
| TA-tktpl | KobCondAsk | touchAsk (settle, stop leg): the custody input tk is a holder UTXO of the token's program (template hash; P2SH kept) | NTA02c | NTA02c(F) | CONFIRMED |
| TA-tkp2sh | KobCondAsk | touchAsk (settle, stop leg): the custody input tk is the P2SH of the holder redeem script read (template hash kept) | NTA02d | NTA02d(F) | CONFIRMED |
| TA-rule | KobCondAsk | settle, unarmed stop leg: the evidence ask quotes <= stopPrice | NTA19, NT3 | NTA19 NT3 | CONFIRMED |
| TU-push08 | KobCondAsk | touch (update: arm on an ask, trail on a bid): the evidence sigscript starts with the fixed 8-byte push of its fill argument (0x08) | NTU10, NTT10, NTT11 | NTU10 NTT10 NTT11 | CONFIRMED |
| TU-npos | KobCondAsk | touch (update: arm on an ask, trail on a bid): the evidence fill argument n > 0 | NTU11 | NTU11 | CONFIRMED |
| TU-tpl | KobCondAsk | touch (update: arm on an ask, trail on a bid): the evidence is an instance of the KobAsk / KobBid template (template hash; P2SH kept) | NTU17, NTT17 | NTU17 NTT17 | CONFIRMED |
| TU-p2sh | KobCondAsk | touch (update: arm on an ask, trail on a bid): the evidence UTXO is the P2SH of the redeem script read (template hash kept) | NTU18, NTT18 | NTU18 NTT18 | CONFIRMED |
| TU-token | KobCondAsk | touch (update: arm on an ask, trail on a bid): the evidence trades this token (tokenCovId) | NTU07b, NTT07 | NTU07b NTT07 | CONFIRMED |
| TU-scale | KobCondAsk | touch (update: arm on an ask, trail on a bid): the evidence quotes per the same scale (prices comparable) | NTU08, NTT08 | NTU08 NTT08 | CONFIRMED |
| TU-mintouch | KobCondAsk | touch (update: arm on an ask, trail on a bid): the evidence fill is >= minTouch base units | NTU09, NTT09 | NTU09 NTT09 | CONFIRMED |
| TU-slope | KobCondAsk | touch (update: arm on an ask, trail on a bid): the evidence does not decay (slope 0) | NTU05, NTT05 | NTU05 NTT05 | CONFIRMED |
| TU-cltv | KobCondAsk | touch (update: arm on an ask, trail on a bid): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTU01, NTU02, NTU03, NTU04, NTT01, NTT03, NTT04 | NTU01 NTU02 NTU03 NTU04 NTT01 NTT03 NTT04 | CONFIRMED |
| TU-interval | KobCondAsk | touch (update: arm on an ask, trail on a bid): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTU04, NTT04 | NTU04 NTT04 | CONFIRMED |
| TU-active | KobCondAsk | touch (update: arm on an ask, trail on a bid): exposure counts the evidence activeFrom | NTU03, NTT03 | NTU03 NTT03 | CONFIRMED |
| TU-tokdaa | KobCondAsk | touch (update: arm on an ask, trail on a bid): exposure counts the evidence ask custody DAA | NTU02 | NTU02 | CONFIRMED |
| TU-tkcov | KobCondAsk | touch (update: arm on an ask, trail on a bid): the custody input tk carries the token covenant id | NTU02b | NTU02b | CONFIRMED |
| TU-owner | KobCondAsk | touch (update: arm on an ask, trail on a bid): the custody input tk is owned by the evidence ask (owner = its covenant id, scheme covenant id) | NTU15, NTU16 | NTU15 NTU16 | CONFIRMED |
| TU-tktpl | KobCondAsk | touch (update: arm on an ask, trail on a bid): the custody input tk is a holder UTXO of the token's program (template hash; P2SH kept) | NTU02c | NTU02c(F) | CONFIRMED |
| TU-tkp2sh | KobCondAsk | touch (update: arm on an ask, trail on a bid): the custody input tk is the P2SH of the holder redeem script read (template hash kept) | NTU02d | NTU02d(F) | CONFIRMED |
| TU-rule | KobCondAsk | update, arm: the evidence ask quotes <= stopPrice | NTU19 | NTU19 | CONFIRMED |
| TT-k | KobCondAsk | update, trail: the evidence bid justifies k >= 1 steps (rp >= stop + step + gap; k = 0 drains keeperTip and resets trailWait, k < 0 moves the stop down) | NTT19, NTT19b | NTT19 NTT19b | CONFIRMED |
| TT-cap | KobCondAsk | update, trail: the ratchet is capped below tpPrice | NT18b | NT18b | CONFIRMED [positive scenarios rejected: S15c] |
| TT-wait | KobCondAsk | update, trail: at most once per trailWait DAA (CSV) | NT15 | NT15 | CONFIRMED |
| TT-step | KobCondAsk | update, trail: trailStep > 0 (a non-trailing order divides by trailStep = 0 and fails anyway) | stay rejected: NT19, NTU06 |  | held (defence in depth) |
| TU-armed | KobCondAsk | update only while unarmed | NT14 | NT14 | CONFIRMED |
| TU-own | KobCondAsk | update spends no token input owned by the order (bounded scan; the evidence fill brings foreign token inputs) | NTU23, NTT23, NT13 | NTU23 NTT23 NT13 | CONFIRMED |
| TI-push08 | KobIfdBid | touchBid (buy-stop entry): the evidence sigscript starts with the fixed 8-byte push of its fill argument (0x08) | NTI10, NTJ10, NTJ11 | NTI10 NTI11(F) NTJ10 NTJ11 | CONFIRMED |
| TI-npos | KobIfdBid | touchBid (buy-stop entry): the evidence fill argument n > 0 | NTI11b, NTJ11b | NTI11b(F) NTJ11b(F) | CONFIRMED |
| TI-tpl | KobIfdBid | touchBid (buy-stop entry): the evidence is an instance of the KobAsk / KobBid template (template hash; P2SH kept) | NTI17, NTJ17 | NTI17 NTJ17 | CONFIRMED |
| TI-p2sh | KobIfdBid | touchBid (buy-stop entry): the evidence UTXO is the P2SH of the redeem script read (template hash kept) | NTI18, NTJ18 | NTI18 NTJ18 | CONFIRMED |
| TI-token | KobIfdBid | touchBid (buy-stop entry): the evidence trades this token (tokenCovId) | NTI07, NTJ07 | NTI07 NTJ07 | CONFIRMED |
| TI-scale | KobIfdBid | touchBid (buy-stop entry): the evidence quotes per the same scale (prices comparable) | NTI08, NTJ08 | NTI08 NTJ08 | CONFIRMED |
| TI-mintouch | KobIfdBid | touchBid (buy-stop entry): the evidence fill is >= minTouch base units | NTI09, NTJ09 | NTI09 NTJ09 | CONFIRMED |
| TI-slope | KobIfdBid | touchBid (buy-stop entry): the evidence does not decay (slope 0) | NTI05, NTJ05 | NTI05 NTJ05 | CONFIRMED |
| TI-cltv | KobIfdBid | touchBid (buy-stop entry): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTI01, NTI03, NTI04, NTJ01, NTJ03, NTJ04 | NTI01 NTI03 NTI04 NTJ01 NTJ03 NTJ04 | CONFIRMED |
| TI-interval | KobIfdBid | touchBid (buy-stop entry): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTI04, NTJ04 | NTI04 NTJ04 | CONFIRMED |
| TI-active | KobIfdBid | touchBid (buy-stop entry): exposure counts the evidence activeFrom | NTI03, NTJ03 | NTI03 NTJ03 | CONFIRMED |
| TI-auc | KobIfdBid | buy-stop entry armed inside its fill with an auction: the fill pays the stop (the auction opens at the trigger) | NI16b | NI16b | CONFIRMED |
| TI-rule | KobIfdBid | fill, unarmed stop entry: the evidence bid quotes >= entryStop | NTI19 | NTI19 | CONFIRMED |
| TJ-rule | KobIfdBid | update: the evidence bid quotes >= entryStop | NTJ19 | NTJ19 | CONFIRMED |
| TJ-own | KobIfdBid | update spends no token input owned by the entry (bounded scan; the evidence fill brings foreign token inputs) | NTJ23 | NTJ23 | CONFIRMED |
| TJ-amount | KobIfdBid | update: the entry has an amount to fill (an empty repeating entry would pay keeperTip for a useless arm) | NTJ24 | NTJ24 | CONFIRMED |
| TJ-limit | KobIfdBid | update: the stop is on the limit's side (entryStop <= price; else no fill can follow the arm) | NTJ25 | NTJ25 | CONFIRMED |
| TJ-origin | KobIfdBid | merge into an entry armed by update (armed 1, band): the continuation records the band origin (else the auction restarts) | NRP29 | NRP29 | CONFIRMED [positive scenarios rejected: RP11] |
| V-A-minfill | KobAsk | minFill: n >= minFill unless the fill takes everything left | V3A01 | V3A01 | CONFIRMED |
| V-A-ceil | KobAsk | the maker's proceeds are rounded up (quoteOf c = scale - 1 -> 0) | V3A02 | V3A02 | CONFIRMED |
| V-A-tip | KobAsk | the price (also a decayed one) covers the tip: p >= tip (else the all-in proceeds are negative) | V3A03, V3A04 | V3A03 V3A04 | CONFIRMED |
| V-A-custody | KobAsk | the custody holds exactly amountLeft base units (a dust UTXO sent to the ask cannot stand in for it; NS1 is also refused by the stray scan) | NS2 | NS2 | CONFIRMED |
| V-B-minfill | KobBid | minFill: n >= minFill unless the bid ends (less than one minimum fill of buying power left) | V3B01 | V3B01 | CONFIRMED |
| V-B-minpos | KobBid | minFill > 0 (a zero minimum would let fills of one base unit drain the escrow into delivery carriers) | V3B02 | V3B02 | CONFIRMED |
| V-B-floor | KobBid | the maker's spend is rounded down (quoteOf c = 0 -> scale - 1) | V3B03, V3B03b | V3B03 V3B03b | CONFIRMED |
| V-B-ceil | KobBid | the escrow budget a fill consumes is rounded up (quoteOf c = scale - 1 -> 0): split fills never buy more than the escrow funds | V3B04 | V3B04 | CONFIRMED [positive scenarios rejected: V3B03+] |
| V-B-fok | KobBid | FOK: the fill leaves less buying power than one minimum fill | V3B06 | V3B06 | CONFIRMED |
| V-CA-minfill | KobCondAsk | minFill: n >= minFill unless the fill takes everything left | V3CA01 | V3CA01 | CONFIRMED |
| V-CA-ceil | KobCondAsk | the maker's proceeds are rounded up (quoteOf c = scale - 1 -> 0) | V3CA02 | V3CA02 | CONFIRMED |
| V-CA-tip | KobCondAsk | the leg price covers the tip: legPrice >= tip (TP leg, stop leg, stop auction) | V3CA03a, V3CA03b, V3CA03c | V3CA03a V3CA03b V3CA03c | CONFIRMED |
| V-CA-rb | KobCondAsk | repeat merge: the budget returned to the entry is rounded up (quoteOf c = scale - 1 -> 0; the entry rounds the same way) | V3CA04 | V3CA04 | CONFIRMED [positive scenarios rejected: V3IB05+ V3CA04+] |
| V-CA-08 | KobCondAsk | repeat merge: the entry's sigscript starts with 0x08 (V3CA08: the merge argument as a 9-byte push, bytes [1..9) = the merge value; the entry refuses its own 9-byte argument, so the exit's flip is input-only) (NRP28, a minimal index push, is also refused by the merge-value comparison; V3CA07, a negative 8-byte index, by the entry's own update; V-CA-08x removes both) | V3CA08; stay rejected: V3CA07, NRP28 | V3CA08(F) | CONFIRMED |
| V-CA-08x | KobCondAsk | repeat merge: the 0x08 first byte and the merge value of the entry's sigscript removed together (the entry's arming update read as a merge) | NRP28 | NRP14 NRP19(F) NRP28 | CONFIRMED |
| V-CA-parent | KobCondAsk | update never next to the repeat entry (OpCovInputCount(parent) == 0): an 8-byte ev push would alias the merge amount (NRP22, a minimal push, is also refused by the entry's 0x08 check) | V3CA05; stay rejected: NRP22 | V3CA05 | CONFIRMED |
| V-CA-mergek | KobCondAsk | re-arm: the exit sells n < 2^53 (the merge argument -(self * 2^53 + n) names this input and n only then: an exit of 2^53 + m at input i reads the merge of the booked exit at input i + 1 as its own) | V3CA09 | V3CA09 | CONFIRMED |
| V-IB-minfill | KobIfdBid | minFill: n >= minFill unless the fill takes everything left | V3IB01, NI18 | V3IB01 NI18 | CONFIRMED |
| V-IB-floor | KobIfdBid | the maker's spend is rounded down (quoteOf c = 0 -> scale - 1) | V3IB02, V3IB02b | V3IB02 V3IB02b | CONFIRMED |
| V-IB-mergek | KobIfdBid | booking: a booked exit amount is below 2^53 (the merge argument -(k * 2^53 + m) must name it) | V3IB03 | V3IB03 | CONFIRMED |
| V-IB-mceil | KobIfdBid | merge floor: the budget of m is rounded up (quoteOf c = scale - 1 -> 0; the exit rounds the same way) | V3IB05 | V3IB05 | CONFIRMED |
| V-IB-08 | KobIfdBid | merge: the exit's sigscript starts with 0x08 (a refund whose nb = 0 is pushed with OP_PUSHDATA1 reads 8 at [1..9)) (NRP22: the exit's minimal update push also fails the amount comparison) | V3IB06; stay rejected: NRP22 | V3IB06 | CONFIRMED |

Collateral flips (scenarios accepted besides the expected ones):

- A3: NRP19
- B5: NRP3 NRP3b NRP16 NRP17b NRP18 NRP29
- FXL3b: NRP20
- TI-push08: NTI11
- V-CA-08x: NRP14 NRP19

## kcc20-buy

KCC-20 v3; test binary `kob_v2_buy_tests`; contracts `contracts/v2`. 97/97 confirmed, 4/4 defence-in-depth held (expected NOT to flip).
Baseline (no mutation): v3_buy_merge_base clean, v2_buy_touch_cond_settle clean, v2_buy_touch_cond_settle_3x3 clean, v2_buy_lifecycle clean, v2_buy_touch_cond_arm clean, v2_buy_touch_cond_trail clean, v2_buy_repeat_ifd_attacks clean, v2_buy_negative clean, v2_buy_touch_cond_arm_3x3 clean, v2_buy_touch_cond_trail_3x3 clean, v2_buy_touch_ifd_arm clean, v2_buy_touch_ifd_fill clean, v3_buy_min_fill clean, v2_buy_touch_ifd_fill_3x3 clean, v3_buy_rounding clean, v3_buy_tip clean, v3_buy_merge_push clean, empty_repeating_ifda_refund_is_refused clean, v2_buy_touch_ifd_arm_3x3 clean.

| id | contract | check ablated | expected | flipped | status |
|---|---|---|---|---|---|
| C1 | KobCondBid | TP without the entry only from rptUntil | NRPB10 | NRPB10 | CONFIRMED |
| C2 | KobCondBid | stop-leg fill refuses the entry (present == 0) | NRPB13 | NRPB13 | CONFIRMED |
| C3 | KobCondBid | entry merge argument names this exit and n (NRPB34, an arming update of the entry named as the merge, is also refused by the v3 0x08 first-byte check: held here, flipped by C3b) | NRPB14; stay rejected: NRPB34 | NRPB14 | CONFIRMED |
| C3b | KobCondBid | both checks on the entry pin removed (0x08 first byte and the merge value): an arming update of the entry is read as the merge | NRPB34 | NRPB14 NRPB34 | CONFIRMED |
| C4 | KobCondBid | maker profit >= ceil proceeds - spend | NRPB11 | NRPB11 | CONFIRMED |
| C5 | KobCondBid | exit continuation keep >= in - proceeds - ceil prefund (NV3CB13: a one-sompi-short continuation at a non-multiple amount) | NRPB12, NV3CB13 | NRPB12 NV3CB13 | CONFIRMED |
| D1 | KobIfdAsk | exit is a genuine KobCondBid (template + P2SH check) (the crafted input also fails the exit-terms comparison, confirmed on its own by FXL3c/d, so removing the template check alone holds) | stay rejected: NRPB21 |  | held (defence in depth) |
| D2 | KobIfdAsk | exit's parent = this entry (the parent check is part of the exit-tail comparison with rptPrice; a plain exit also fails on rptPrice, so removing the parent alone holds) | stay rejected: NRPB20 |  | held (defence in depth) |
| D3 | KobIfdAsk | merge: partial buy-back re-armed with ceil(m * prefund) | NRPB15 | NRPB15 NRPB31 | CONFIRMED |
| D3b | KobIfdAsk | merge: sell-out leftovers come back (floor with the exit value; NV3IA13: a one-sompi-short sell-out continuation at a non-multiple amount) | NRPB16, NV3IA13 | NRPB16 NV3IA13 | CONFIRMED |
| D4 | KobIfdAsk | merge: custody carrier kept (tokOut value >= keepCarrier) | NRPB16b | NRPB16b NRPB16c | CONFIRMED |
| D5 | KobIfdAsk | token output pin (custody / remainder / refund state), condition inverted | NRPB18 | NRPB17 NRPB18 | CONFIRMED |
| D6 | KobIfdAsk | continuation SPK equality (limit, cycle count, amount) | NRPB19 | NRPB3 NRPB19 NRPB19b NRPB19c NRPB19d NRPB35 | CONFIRMED |
| D6b | KobIfdAsk | empty stop entry re-arms unarmed (newArmed = 0) | NRPB19d | NRPB19d | CONFIRMED |
| D7 | KobIfdAsk | an empty entry has no custody (custody = 0 - 1 statement removed) | NRPB24b | NRPB24b | CONFIRMED |
| D7b | KobIfdAsk | stray scan in merge (noStrays(selfId, custody)) | NRPB24 | NRPB24 NRPB24b | CONFIRMED |
| D8 | KobIfdAsk | custody holds exactly amountLeft base units (amount check) | NRPB25 | NRPB25 | CONFIRMED |
| D9 | KobIfdAsk | booking: the exit's rptUntil counts from the entry UTXO's DAA (its last activity), never from the filler's t (the cycle dated by t instead) | NRPB6 | NRPB6 | CONFIRMED |
| D10 | KobIfdAsk | repeating entry never terminates on a fill | NRPB7 | NRPB7 | CONFIRMED |
| D11 | KobIfdAsk | sold out: entry keeps the custody carrier | NRPB8 | NRPB8 | CONFIRMED |
| D12 | KobIfdAsk | close only with no amount left | NRPB26 | NRPB26 | CONFIRMED |
| D13 | KobIfdAsk | close refuses token inputs | NRPB28 | NRPB28 | CONFIRMED |
| M2 | KobCondBid | control: buy stop auction time >= arming origin (require(t >= newArmed)) | NB29 | NB29 | CONFIRMED |
| FXL3c | KobIfdAsk | the merge requires the exit terms [0..223) (maker, legs) to be the committed exitState | NRPB29 | NRPB29 | CONFIRMED |
| FXL3d | KobIfdAsk | the merge requires parent = this entry, rptPrice and rptPre = its own | NRPB30 | NRPB20 NRPB30 | CONFIRMED |
| FXL3e | KobIfdAsk | the merge requires the exit to run its settle with n = m (sigscript [1..9)); defence in depth in v3: a cancel (NRPB32) or an update (NRPB33) of the exit never starts with the 0x08 push either, so the first-byte check alone still refuses them (both removed: FXL3g) | stay rejected: NRPB32, NRPB33 |  | held (defence in depth) |
| FXL3g | KobIfdAsk | both checks on the exit input removed (0x08 first byte and n = m): a cancel (NRPB32) or an arming update (NRPB33; its own parent guard still refuses it, so the entry input alone flips) of the exit is read as its settle | NRPB32, NRPB33 | NRPB32 NRPB33(F) | CONFIRMED |
| FXL3f | KobIfdAsk | a sell-out merge never costs the entry (floor = max(prefund, exit value - proceeds)) | NRPB31 | NRPB31 | CONFIRMED |
| TB-push08 | KobCondBid | touchBid (settle, buy-stop leg): the evidence sigscript starts with the fixed 8-byte push of its fill argument (0x08) | NTB10 | NTB10 NTB11(F) | CONFIRMED |
| TB-npos | KobCondBid | touchBid (settle, buy-stop leg): the evidence fill argument n > 0 | NTB11b | NTB11b(F) | CONFIRMED |
| TB-tpl | KobCondBid | touchBid (settle, buy-stop leg): the evidence is an instance of the KobAsk / KobBid template (template hash; P2SH kept) | NTB17 | NTB17 | CONFIRMED |
| TB-p2sh | KobCondBid | touchBid (settle, buy-stop leg): the evidence UTXO is the P2SH of the redeem script read (template hash kept) | NTB18 | NTB18 | CONFIRMED |
| TB-token | KobCondBid | touchBid (settle, buy-stop leg): the evidence trades this token (tokenCovId) | NTB07 | NTB07 | CONFIRMED |
| TB-scale | KobCondBid | touchBid (settle, buy-stop leg): the evidence quotes per the same scale (prices comparable) | NTB08 | NTB08 | CONFIRMED |
| TB-mintouch | KobCondBid | touchBid (settle, buy-stop leg): the evidence fill is >= minTouch base units | NTB09 | NTB09 | CONFIRMED |
| TB-slope | KobCondBid | touchBid (settle, buy-stop leg): the evidence does not decay (slope 0) | NTB05 | NTB05 | CONFIRMED |
| TB-cltv | KobCondBid | touchBid (settle, buy-stop leg): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTB01, NTB03, NTB04 | NTB01 NTB03 NTB04 | CONFIRMED |
| TB-interval | KobCondBid | touchBid (settle, buy-stop leg): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTB04 | NTB04 | CONFIRMED |
| TB-active | KobCondBid | touchBid (settle, buy-stop leg): exposure counts the evidence activeFrom | NTB03 | NTB03 | CONFIRMED |
| TB-rule | KobCondBid | settle, unarmed buy-stop leg: the evidence bid quotes >= stopPrice | NTB19, NB13 | NTB19 NB13 | CONFIRMED |
| TV-push08 | KobCondBid | touch (update: arm on a bid, trail on an ask): the evidence sigscript starts with the fixed 8-byte push of its fill argument (0x08) | NTV10, NTV11, NTW10 | NTV10 NTV11 NTW10 | CONFIRMED |
| TV-npos | KobCondBid | touch (update: arm on a bid, trail on an ask): the evidence fill argument n > 0 | NTW11 | NTW11 | CONFIRMED |
| TV-tpl | KobCondBid | touch (update: arm on a bid, trail on an ask): the evidence is an instance of the KobAsk / KobBid template (template hash; P2SH kept) | NTV17, NTW17 | NTV17 NTW17 | CONFIRMED |
| TV-p2sh | KobCondBid | touch (update: arm on a bid, trail on an ask): the evidence UTXO is the P2SH of the redeem script read (template hash kept) | NTV18, NTW18 | NTV18 NTW18 | CONFIRMED |
| TV-token | KobCondBid | touch (update: arm on a bid, trail on an ask): the evidence trades this token (tokenCovId) | NTV07, NTW07b | NTV07 NTW07b | CONFIRMED |
| TV-scale | KobCondBid | touch (update: arm on a bid, trail on an ask): the evidence quotes per the same scale (prices comparable) | NTV08, NTW08 | NTV08 NTW08 | CONFIRMED |
| TV-mintouch | KobCondBid | touch (update: arm on a bid, trail on an ask): the evidence fill is >= minTouch base units | NTV09, NTW09 | NTV09 NTW09 | CONFIRMED |
| TV-slope | KobCondBid | touch (update: arm on a bid, trail on an ask): the evidence does not decay (slope 0) | NTV05, NTW05 | NTV05 NTW05 | CONFIRMED |
| TV-cltv | KobCondBid | touch (update: arm on a bid, trail on an ask): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTV01, NTV03, NTV04, NTW01, NTW02, NTW03, NTW04 | NTV01 NTV03 NTV04 NTW01 NTW02 NTW03 NTW04 | CONFIRMED |
| TV-interval | KobCondBid | touch (update: arm on a bid, trail on an ask): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTV04, NTW04 | NTV04 NTW04 | CONFIRMED |
| TV-active | KobCondBid | touch (update: arm on a bid, trail on an ask): exposure counts the evidence activeFrom | NTV03, NTW03 | NTV03 NTW03 | CONFIRMED |
| TV-tokdaa | KobCondBid | touch (update: arm on a bid, trail on an ask): exposure counts the evidence ask custody DAA | NTW02 | NTW02 | CONFIRMED |
| TV-tkcov | KobCondBid | touch (update: arm on a bid, trail on an ask): the custody input tk carries the token covenant id | NTW02b | NTW02b | CONFIRMED |
| TV-owner | KobCondBid | touch (update: arm on a bid, trail on an ask): the custody input tk is owned by the evidence ask (owner = its covenant id, scheme covenant id) | NTW15, NTW16 | NTW15 NTW16 | CONFIRMED |
| TV-tktpl | KobCondBid | touch (update: arm on a bid, trail on an ask): the custody input tk is a holder UTXO of the token's program (template hash; P2SH kept) | NTW02c | NTW02c(F) | CONFIRMED |
| TV-tkp2sh | KobCondBid | touch (update: arm on a bid, trail on an ask): the custody input tk is the P2SH of the holder redeem script read (template hash kept) | NTW02d | NTW02d(F) | CONFIRMED |
| TV-rule | KobCondBid | update, arm: the evidence bid quotes >= stopPrice | NTV19 | NTV19 | CONFIRMED |
| TW-k | KobCondBid | update, trail: the evidence ask justifies k >= 1 steps (k = 0 drains keeperTip and resets trailWait, k < 0 moves the stop up) | NTW19, NTW19b | NTW19 NTW19b | CONFIRMED |
| TW-cap | KobCondBid | update, trail: the ratchet stays above the limit leg (tpPrice) and 0 | NB22b | NB22b | CONFIRMED [positive scenarios rejected: B5c] |
| TW-wait | KobCondBid | update, trail: at most once per trailWait DAA (CSV) | NB20 | NB20 | CONFIRMED |
| TW-step | KobCondBid | update, trail: trailStep > 0 (a non-trailing order divides by trailStep = 0 and fails anyway) | stay rejected: NB24, NTV06 |  | held (defence in depth) |
| TV-armed | KobCondBid | update only while unarmed | NB19 | NB19 | CONFIRMED |
| TV-own | KobCondBid | update spends no token input owned by the order (bounded scan; the evidence fill brings foreign token inputs) | NTV23, NTW23, NS12 | NTV23 NTW23 NS12 | CONFIRMED |
| TK-push08 | KobIfdAsk | touchAsk (sell-stop entry): the evidence sigscript starts with the fixed 8-byte push of its fill argument (0x08) | NTK10, NTL10 | NTK10 NTL10 | CONFIRMED |
| TK-npos | KobIfdAsk | touchAsk (sell-stop entry): the evidence fill argument n > 0 | NTK11, NTL11 | NTK11 NTL11 | CONFIRMED |
| TK-tpl | KobIfdAsk | touchAsk (sell-stop entry): the evidence is an instance of the KobAsk / KobBid template (template hash; P2SH kept) | NTK17, NTL17 | NTK17 NTL17 | CONFIRMED |
| TK-p2sh | KobIfdAsk | touchAsk (sell-stop entry): the evidence UTXO is the P2SH of the redeem script read (template hash kept) | NTK18, NTL18 | NTK18 NTL18 | CONFIRMED |
| TK-token | KobIfdAsk | touchAsk (sell-stop entry): the evidence trades this token (tokenCovId) | NTK07b, NTL07b | NTK07b NTL07b | CONFIRMED |
| TK-scale | KobIfdAsk | touchAsk (sell-stop entry): the evidence quotes per the same scale (prices comparable) | NTK08, NTL08 | NTK08 NTL08 | CONFIRMED |
| TK-mintouch | KobIfdAsk | touchAsk (sell-stop entry): the evidence fill is >= minTouch base units | NTK09, NTL09 | NTK09 NTL09 | CONFIRMED |
| TK-slope | KobIfdAsk | touchAsk (sell-stop entry): the evidence does not decay (slope 0) | NTK05, NTL05 | NTK05 NTL05 | CONFIRMED |
| TK-cltv | KobIfdAsk | touchAsk (sell-stop entry): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTK01, NTK02, NTK03, NTK04, NTL01, NTL02, NTL03, NTL04 | NTK01 NTK02 NTK03 NTK04 NTL01 NTL02 NTL03 NTL04 | CONFIRMED |
| TK-interval | KobIfdAsk | touchAsk (sell-stop entry): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTK04, NTL04 | NTK04 NTL04 | CONFIRMED |
| TK-active | KobIfdAsk | touchAsk (sell-stop entry): exposure counts the evidence activeFrom | NTK03, NTL03 | NTK03 NTL03 | CONFIRMED |
| TK-tokdaa | KobIfdAsk | touchAsk (sell-stop entry): exposure counts the evidence ask custody DAA | NTK02, NTL02 | NTK02 NTL02 | CONFIRMED |
| TK-tkcov | KobIfdAsk | touchAsk (sell-stop entry): the custody input tk carries the token covenant id | NTK02b, NTL02b | NTK02b NTL02b | CONFIRMED |
| TK-owner | KobIfdAsk | touchAsk (sell-stop entry): the custody input tk is owned by the evidence ask (owner = its covenant id, scheme covenant id) | NTK15, NTK16, NTL15, NTL16 | NTK15 NTK16 NTL15 NTL16 | CONFIRMED |
| TK-tktpl | KobIfdAsk | touchAsk (sell-stop entry): the custody input tk is a holder UTXO of the token's program (template hash; P2SH kept) | NTK02c, NTL02c | NTK02c(F) NTL02c(F) | CONFIRMED |
| TK-tkp2sh | KobIfdAsk | touchAsk (sell-stop entry): the custody input tk is the P2SH of the holder redeem script read (template hash kept) | NTK02d, NTL02d | NTK02d(F) NTL02d(F) | CONFIRMED |
| TK-auc | KobIfdAsk | sell-stop entry armed inside its fill with an auction: the fill sells at the stop (the auction opens at the trigger) | NI18b | NI18b | CONFIRMED |
| TK-rule | KobIfdAsk | settle, unarmed stop entry: the evidence ask quotes <= entryStop | NTK19 | NTK19 | CONFIRMED |
| TL-rule | KobIfdAsk | update: the evidence ask quotes <= entryStop | NTL19 | NTL19 | CONFIRMED |
| TL-own | KobIfdAsk | update spends no token input owned by the entry (bounded scan; the evidence fill brings foreign token inputs) | NTL23, NI22 | NTL23 NI22 | CONFIRMED |
| TL-amount | KobIfdAsk | update: the entry has a positive amount to fill (an empty repeating entry would pay keeperTip for a useless arm) | NTL24 | NTL24 | CONFIRMED |
| TL-limit | KobIfdAsk | update: the stop is on the limit's side (entryStop >= price; else no fill can follow the arm) | NTL25 | NTL25 | CONFIRMED |
| TL-origin | KobIfdAsk | merge into an entry armed by update (armed 1, band): the continuation records the band origin (else the auction restarts) | NRPB35 | NRPB35 | CONFIRMED [positive scenarios rejected: RPB10] |
| CB-minfill | KobCondBid | minFill: n >= minFill unless the fill takes all that is left | NV3CB01 | NV3CB01 | CONFIRMED |
| CB-spend-floor | KobCondBid | bid spend is the floor of n * (legPrice + tip) / scale (c = 0, maker pays the floor) | NV3CB10, NV3CB11 | NV3CB10 NV3CB11 | CONFIRMED |
| CB-rearm-proceeds-ceil | KobCondBid | repeat merge: the maker receives the CEIL of its proceeds (c = scale - 1) | NV3CB12 | NV3CB12 | CONFIRMED [positive scenarios rejected: V3CB20] |
| CB-update-parent | KobCondBid | update (arm / trail) may not run next to the repeat entry (OpCovInputCount(parent) == 0) | NV3CB41 | NV3CB41 | CONFIRMED |
| CB-merge-0x08 | KobCondBid | repeat merge: the entry pin's fill argument must start with the 8-byte push 0x08 (a non-minimal 9-byte push is refused) | NV3CB40 | NV3CB40(F) | CONFIRMED |
| CB-rearm-mergek | KobCondBid | re-arm: n < MERGE_K (2^53), so the merge argument -(self * 2^53 + n) names this input and n (a TP fill of 2^53 + m would read as the merge of the booked exit at self + 1) | NV3CB43 | NV3CB43 | CONFIRMED |
| IA-price-tip | KobIfdAsk | the all-in price is at least the tip (price >= tip), so a booked exit's rptPrice = price - tip is never negative | NV3IA20, NV3IA21 | NV3IA20 NV3IA21 | CONFIRMED |
| IA-minfill | KobIfdAsk | minFill: n >= minFill unless the fill sells out | NV3IA01 | NV3IA01 | CONFIRMED |
| IA-proceeds-ceil | KobIfdAsk | the exit receives the CEIL of the seller proceeds (c = scale - 1) | NV3IA10 | NV3IA10 NV3IA11 | CONFIRMED |
| IA-pre-ceil | KobIfdAsk | the exit receives the CEIL of the prefund (c = scale - 1; pre also sets the entry floor, so the attack keeps the sompi in the entry) | NV3IA11 | NV3IA11 | CONFIRMED [positive scenarios rejected: V3IA10] |
| IA-merge-prefund-ceil | KobIfdAsk | repeat merge floor: the entry keeps at least in + CEIL(m * prefund / scale) | NV3IA12 | NV3IA12 | CONFIRMED |
| IA-booking-mergek | KobIfdAsk | a booked exit's amount stays below MERGE_K (2^53), so it fits the merge argument without overflowing into k | NV3IA31 | NV3IA31 | CONFIRMED |
| IA-merge-0x08 | KobIfdAsk | repeat merge: the exit's fill argument (input k) must start with the 8-byte push 0x08 (a non-minimal 9-byte push is refused) | NV3IA40 | NV3IA40(F) | CONFIRMED |
| IA-merge-scale | KobIfdAsk | repeat merge: the committed exit quotes at the entry's scale (an exit at scale 999 refused: the two would round the merge differently) | NRPB36 | NRPB36 | CONFIRMED |
| IA-refund-held | KobIfdAsk | refund (settle nb = 0) only while tokens are held: an empty repeating entry with a zero-amount custody would pin no output, so close() is its end | SR1b, SR1c | SR1b SR1c | CONFIRMED |

Collateral flips (scenarios accepted besides the expected ones):

- C3b: NRPB14
- D3: NRPB31
- D4: NRPB16c
- D5: NRPB17
- D6: NRPB3 NRPB19b NRPB19c NRPB19d NRPB35
- D7b: NRPB24b
- FXL3d: NRPB20
- TB-push08: NTB11
- IA-proceeds-ceil: NV3IA11

## kron-sell

KRON adapter (protocol v3, sell-side contracts); test binary `kob_kron_v2_tests`; contracts `contracts/adapters/kron/v2`. 145/145 confirmed, 3/3 defence-in-depth held (expected NOT to flip).
Baseline (no mutation): v2_lifecycle_stray_custody clean, v2_lifecycle_auctions_and_kill clean, v2_kron_v23_extra_attacks clean, v2_lifecycle_stop_auction_keeper_trailing clean, v2_negative_trigger_manipulation clean, v3_min_fill clean, v3_merge_push clean, v2_kron_fix_pass_regressions clean, v2_lifecycle_ifd_stop_entry clean, v2_touch_ifd_arm clean, v2_touch_cond_settle clean, v2_repeat_ifd_attacks clean, v2_touch_cond_arm clean, v3_tip clean, v3_merge_limits clean, v3_rounding clean, v2_touch_cond_trail clean, v2_touch_ifd_fill clean.

| id | contract | check ablated | expected | flipped 2433 | flipped 2732 | status |
|---|---|---|---|---|---|---|
| A1 | KobAskKron | decayStep > 0 | KS9 | KS9 | KS9 | CONFIRMED |
| A2 | KobAskKron | per-slice decay origin | NV10, NV11 | NV10 NV11 | NV10 NV11 | CONFIRMED |
| A3 | KobAskKron | t >= origin | KS10 | KS10 | KS10 | CONFIRMED |
| A4 | KobAskKron | auction time CLTV | NM3 | NM3 | NM3 | CONFIRMED |
| A5 | KobAskKron | auction price formula (always the bound) | NM1 | NM1 NV10 | NM1 NV10 | CONFIRMED |
| A6 | KobAskKron | IOC/FOK kill one DAA early | NC8, NC8b | NC8 NC8b | NC8 NC8b | CONFIRMED |
| A7 | KobAskKron | kill counted from UTXO DAA, not activeFrom | NC8b | NC8b | NC8b | CONFIRMED |
| A8 | KobAskKron | GTC also killable | NC9 | NC9 | NC9 | CONFIRMED |
| A9 | KobAskKron | exact custody amount (custody == amountLeft) | NS2 | NS2 | NS2 | CONFIRMED |
| A10 | KobAskKron | stray scan | NS3, NS4 | NS3 NS4 | NS3 NS4 | CONFIRMED |
| A11 | KobAskKron | exact custody + stray scan (NS1 is double-locked) | NS1, NS2, NS3, NS4 | NS1 NS2 NS3 NS4 | NS1 NS2 NS3 NS4 | CONFIRMED |
| A12 | KobAskKron | continuation amountLeft not decremented | KS2 | KS2 | KS2 | CONFIRMED [positive scenarios rejected: KS1a] |
| B1 | KobBidKron | IOC/FOK kill one DAA early | NC10 | NC10 | NC10 | CONFIRMED |
| B2 | KobBidKron | GTC also killable | NC10b | NC10b | NC10b | CONFIRMED |
| B3 | KobBidKron | fill stray scan | NS5 | NS5 | NS5 | CONFIRMED |
| B4 | KobBidKron | refund refuses token inputs | NS6 | NS6 | NS6 | CONFIRMED |
| B5 | KobBidKron | rising price formula (always the bound) | NM4 | NM4 | NM4 | CONFIRMED |
| B6 | KobBidKron | decayStep > 0 | KS5 | KS5 | KS5 | CONFIRMED |
| B7 | KobBidKron | per-slice rising origin | KS6 | KS6 KS6b | KS6 KS6b | CONFIRMED |
| C1 | KobCondAskKron | auction time CLTV | NA2 | NA2 | NA2 | CONFIRMED |
| C2 | KobCondAskKron | t >= arming origin | NA3 | NA3 | NA3 | CONFIRMED |
| C3 | KobCondAskKron | stop auction ramp (always the floor) | NA1 | NA1 | NA1 | CONFIRMED |
| C4 | KobCondAskKron | arm + fill pays the stop | NA5 | NA5 | NA5 | CONFIRMED |
| C5 | KobCondAskKron | origin recorded at the first auction fill | NA4 | NA4 | NA4 | CONFIRMED [positive scenarios rejected: S17 S17c S17e] |
| C6 | KobCondAskKron | continuation SPK equality (settle) | NA6 | NA4 NA6 | NA4 NA6 | CONFIRMED |
| C7 | KobCondAskKron | keeper takes at most keeperTip | NK1 | NK1 | NK1 | CONFIRMED |
| C8 | KobCondAskKron | keeperTip >= 0 | NK2 | NK2 | NK2 | CONFIRMED |
| C9 | KobCondAskKron | trail capped below TP | NT18b | NT18b | NT18b | CONFIRMED [positive scenarios rejected: S15c] |
| C10 | KobCondAskKron | trail exactly k steps (one step) | NT17 | NT17 | NT17 | CONFIRMED |
| C11 | KobCondAskKron | trail exactly k steps (k + 1) | NT17b | NT17b | NT17b | CONFIRMED |
| C12 | KobCondAskKron | exact custody amount (custody == amountLeft) | NS7b | NS7b | NS7b | CONFIRMED |
| C13 | KobCondAskKron | stray scan | NS7 | NS7 | NS7 | CONFIRMED |
| C14 | KobCondAskKron | TP without the entry only from rptUntil | NRP10 | NRP10 | NRP10 | CONFIRMED |
| C15 | KobCondAskKron | stop leg refuses the entry | NRP13 | NRP13 | NRP13 | CONFIRMED |
| C16 | KobCondAskKron | entry's merge argument names this exit and n (NRP28, an arming update of the entry, also fails the 0x08 check: see C16b) | NRP14; stay rejected: NRP28 | NRP14 NRP19(F) | NRP14 NRP19(F) | CONFIRMED |
| C16b | KobCondAskKron | merge argument value and the entry's leading 0x08 both removed: an arming update of the entry passes as its merge | NRP28 | NRP14 NRP19(F) NRP28 | NRP14 NRP19(F) NRP28 | CONFIRMED |
| C17 | KobCondAskKron | merge-n binding on both sides (exit AND entry check removed) | NRP19 | NRP14 NRP19 | NRP14 NRP19 | CONFIRMED |
| C18 | KobCondAskKron | maker payout floor (partial re-arming TP) | NRP11 | NRP11 | NRP11 | CONFIRMED |
| C19 | KobCondAskKron | sell-out: entry gets the exit carriers | NRP12 | NRP12 | NRP12 | CONFIRMED |
| I1 | KobIfdBidKron | unarmed stop entry reads its trigger evidence (touch) | NI13 | NI13 NI14 NI15 NI15b | NI13 NI14 NI15 NI15b | CONFIRMED |
| I5 | KobIfdBidKron | entry auction price ramp | NI16 | NI16 | NI16 | CONFIRMED |
| I6 | KobIfdBidKron | auction origin recorded | NI17 | NI17 | NI17 | CONFIRMED [positive scenarios rejected: S18b] |
| I7 | KobIfdBidKron | armed carried into the continuation | NI17b | NI17b | NI17b | CONFIRMED [positive scenarios rejected: S18 S18c] |
| I8 | KobIfdBidKron | minFill (n >= minFill unless the fill takes everything left) | NI18, V3IB01 | NI18 V3IB01 | NI18 V3IB01 | CONFIRMED |
| I9 | KobIfdBidKron | entryStop <= price | NI19 | NI19 | NI19 | CONFIRMED |
| I10 | KobIfdBidKron | update needs a stop entry | NI20 | NI20 | NI20 | CONFIRMED |
| I11 | KobIfdBidKron | keeper takes at most keeperTip | NI21 | NI21 | NI21 | CONFIRMED |
| I12 | KobIfdBidKron | update continuation SPK equality | NI23 | NI23 | NI23 | CONFIRMED |
| I13 | KobIfdBidKron | fill stray scan | NS8 | NS8 | NS8 | CONFIRMED |
| I14 | KobIfdBidKron | update refuses token inputs owned by the entry (touch: the evidence fill brings foreign token inputs) | KS3, NTJ23 | KS3 NTJ23 | KS3 NTJ23 | CONFIRMED |
| I15 | KobIfdBidKron | refund refuses token inputs | KS4 | KS4 | KS4 | CONFIRMED |
| I16 | KobIfdBidKron | update keeperTip >= 0 | KS8 | KS8 | KS8 | CONFIRMED |
| R1 | KobIfdBidKron | exit is a genuine KobCondAsk (template + P2SH check) (the crafted input also fails the exit-terms comparison, confirmed on its own by FXL3a/b, so removing the template check alone holds) | stay rejected: NRP21 |  |  | held (defence in depth) |
| R2 | KobIfdBidKron | exit's parent = this entry (the parent check is part of the exit-tail comparison with rptPrice; a plain exit also fails on rptPrice, so removing the parent alone holds) | stay rejected: NRP20 |  |  | held (defence in depth) |
| R3 | KobIfdBidKron | exit sells exactly m (NRP22, beside the exit's update, is also refused by the exit's parent guard and the 0x08 check) | V3IB05; stay rejected: NRP22 | V3IB05 | V3IB05 | CONFIRMED |
| R4 | KobIfdBidKron | m > 0 | NRP23 | NRP23 | NRP23 | CONFIRMED |
| R5 | KobIfdBidKron | continuation SPK equality (fill/merge) | NRP16, NRP17, NRP17b, NRP18 | NRP3 NRP3b NRP16 NRP17 NRP17b NRP18 NRP29 | NRP3 NRP3b NRP16 NRP17 NRP17b NRP18 NRP29 | CONFIRMED |
| R6 | KobIfdBidKron | empty stop entry re-arms unarmed | NRP18 | NRP18 | NRP18 | CONFIRMED |
| R7 | KobIfdBidKron | merge floor (the budget of m) | NRP15 | NRP15 | NRP15 | CONFIRMED |
| R8 | KobIfdBidKron | stray scan in merge (shares the fill guard) | NRP24 | NRP24 | NRP24 | CONFIRMED |
| R9 | KobIfdBidKron | booking: the exit's rptUntil counts from the entry UTXO's DAA (its last activity), never from the filler's t (the cycle dated by t instead) | NRP6 | NRP6 | NRP6 | CONFIRMED |
| R10 | KobIfdBidKron | repeating entry never terminates on a fill | NRP7 | NRP7 | NRP7 | CONFIRMED |
| R11 | KobIfdBidKron | continuation floor on a fill | NRP8 | NRP8 | NRP8 | CONFIRMED |
| R12 | KobIfdBidKron | cycle count decremented | NRP3 | NRP3 | NRP3 | CONFIRMED |
| R13 | KobIfdBidKron | exit rptPrice | NRP4 | NRP4 | NRP4 | CONFIRMED |
| R14 | KobIfdBidKron | exit rptUntil | NRP5 | NRP5 | NRP5 | CONFIRMED |
| R15 | KobIfdBidKron | booking writes a plain exit (parent, rptPrice, rptUntil) although re-arms remain | NRP1 | NRP1 | NRP1 | CONFIRMED |
| R16 | KobIfdBidKron | no booking once the re-arms are exhausted (rptAmount > n) | NRP2 | NRP2 | NRP2 | CONFIRMED |
| R17 | KobIfdBidKron | continuation keeps the repeat count | NRP3b | NRP3b | NRP3b | CONFIRMED |
| FXE1 | KobAskKron | the IOC return is positional (tokOut == tokenIn): no shared remainder, no remainder that is also a bid delivery | FXE1, FXE2 | FXE1 FXE2 | FXE1 FXE2 | CONFIRMED |
| FXL3a | KobIfdBidKron | the merge requires the exit terms [0..181) (maker, legs) to be the committed exitState | NRP25, NRP27 | NRP25 NRP27 | NRP25 NRP27 | CONFIRMED |
| FXL3b | KobIfdBidKron | the merge requires parent = this entry and rptPrice = its budget rate | NRP26 | NRP20 NRP26 | NRP20 NRP26 | CONFIRMED |
| V3-I-scale | KobIfdBidKron | merge: the committed exit (exitState, compared with the exit's state) quotes at the entry's scale (else the exit returns budget(m) at its scale and the entry re-arms m at its own) | NRP30 | NRP30 | NRP30 | CONFIRMED |
| TA-push08 | KobCondAskKron | touchAsk (settle, stop leg): the evidence sigscript starts with the fixed 8-byte push of its fill argument (0x08) | NTA10 | NTA10 | NTA10 | CONFIRMED |
| TA-npos | KobCondAskKron | touchAsk (settle, stop leg): the evidence fill argument n > 0 | NTA11 | NTA11 | NTA11 | CONFIRMED |
| TA-tpl | KobCondAskKron | touchAsk (settle, stop leg): the evidence is an instance of the KobAsk / KobBid template (template hash; P2SH kept) | NTA17 | NTA17 | NTA17 | CONFIRMED |
| TA-p2sh | KobCondAskKron | touchAsk (settle, stop leg): the evidence UTXO is the P2SH of the redeem script read (template hash kept) | NTA18 | NTA18 | NTA18 | CONFIRMED |
| TA-token | KobCondAskKron | touchAsk (settle, stop leg): the evidence trades this token (tokenCovId) | NTA07b | NTA07b | NTA07b | CONFIRMED |
| TA-scale | KobCondAskKron | touchAsk (settle, stop leg): the evidence quotes per the same scale (prices comparable) | NTA08 | NTA08 | NTA08 | CONFIRMED |
| TA-mintouch | KobCondAskKron | touchAsk (settle, stop leg): the evidence fill is >= minTouch base units | NTA09, NT5 | NTA09 NT5 | NTA09 NT5 | CONFIRMED |
| TA-slope | KobCondAskKron | touchAsk (settle, stop leg): the evidence does not decay (slope 0) | NTA05 | NTA05 | NTA05 | CONFIRMED |
| TA-cltv | KobCondAskKron | touchAsk (settle, stop leg): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTA01, NTA02, NTA03, NTA04 | NTA01 NTA02 NTA03 NTA04 | NTA01 NTA02 NTA03 NTA04 | CONFIRMED |
| TA-interval | KobCondAskKron | touchAsk (settle, stop leg): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTA04 | NTA04 | NTA04 | CONFIRMED |
| TA-active | KobCondAskKron | touchAsk (settle, stop leg): exposure counts the evidence activeFrom | NTA03 | NTA03 | NTA03 | CONFIRMED |
| TA-tokdaa | KobCondAskKron | touchAsk (settle, stop leg): exposure counts the evidence ask custody DAA | NTA02 | NTA02 | NTA02 | CONFIRMED |
| TA-tkcov | KobCondAskKron | touchAsk (settle, stop leg): the custody input tk carries the token covenant id | NTA02b | NTA02b | NTA02b | CONFIRMED |
| TA-owner | KobCondAskKron | touchAsk (settle, stop leg): the custody input tk is owned by the evidence ask (owner = its covenant id, scheme covenant id) | NTA15, NTA16 | NTA15 NTA16 | NTA15 NTA16 | CONFIRMED |
| TA-tktpl | KobCondAskKron | touchAsk (settle, stop leg): the custody input tk is a holder UTXO of the token's program (template hash; P2SH kept) | NTA02c | NTA02c(F) | NTA02c(F) | CONFIRMED |
| TA-tkp2sh | KobCondAskKron | touchAsk (settle, stop leg): the custody input tk is the P2SH of the holder redeem script read (template hash kept) | NTA02d | NTA02d(F) | NTA02d(F) | CONFIRMED |
| TA-rule | KobCondAskKron | settle, unarmed stop leg: the evidence ask quotes <= stopPrice | NTA19, NT3 | NTA19 NT3 | NTA19 NT3 | CONFIRMED |
| TU-push08 | KobCondAskKron | touch (update: arm on an ask, trail on a bid): the evidence sigscript starts with the fixed 8-byte push of its fill argument (0x08) | NTU10, NTT10, NTT11 | NTU10 NTT10 NTT11 | NTU10 NTT10 NTT11 | CONFIRMED |
| TU-npos | KobCondAskKron | touch (update: arm on an ask, trail on a bid): the evidence fill argument n > 0 | NTU11 | NTU11 | NTU11 | CONFIRMED |
| TU-tpl | KobCondAskKron | touch (update: arm on an ask, trail on a bid): the evidence is an instance of the KobAsk / KobBid template (template hash; P2SH kept) | NTU17, NTT17 | NTU17 NTT17 | NTU17 NTT17 | CONFIRMED |
| TU-p2sh | KobCondAskKron | touch (update: arm on an ask, trail on a bid): the evidence UTXO is the P2SH of the redeem script read (template hash kept) | NTU18, NTT18 | NTU18 NTT18 | NTU18 NTT18 | CONFIRMED |
| TU-token | KobCondAskKron | touch (update: arm on an ask, trail on a bid): the evidence trades this token (tokenCovId) | NTU07b, NTT07 | NTU07b NTT07 | NTU07b NTT07 | CONFIRMED |
| TU-scale | KobCondAskKron | touch (update: arm on an ask, trail on a bid): the evidence quotes per the same scale (prices comparable) | NTU08, NTT08 | NTU08 NTT08 | NTU08 NTT08 | CONFIRMED |
| TU-mintouch | KobCondAskKron | touch (update: arm on an ask, trail on a bid): the evidence fill is >= minTouch base units | NTU09, NTT09 | NTU09 NTT09 | NTU09 NTT09 | CONFIRMED |
| TU-slope | KobCondAskKron | touch (update: arm on an ask, trail on a bid): the evidence does not decay (slope 0) | NTU05, NTT05 | NTU05 NTT05 | NTU05 NTT05 | CONFIRMED |
| TU-cltv | KobCondAskKron | touch (update: arm on an ask, trail on a bid): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTU01, NTU02, NTU03, NTU04, NTT01, NTT03, NTT04 | NTU01 NTU02 NTU03 NTU04 NTT01 NTT03 NTT04 | NTU01 NTU02 NTU03 NTU04 NTT01 NTT03 NTT04 | CONFIRMED |
| TU-interval | KobCondAskKron | touch (update: arm on an ask, trail on a bid): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTU04, NTT04 | NTU04 NTT04 | NTU04 NTT04 | CONFIRMED |
| TU-active | KobCondAskKron | touch (update: arm on an ask, trail on a bid): exposure counts the evidence activeFrom | NTU03, NTT03 | NTU03 NTT03 | NTU03 NTT03 | CONFIRMED |
| TU-tokdaa | KobCondAskKron | touch (update: arm on an ask, trail on a bid): exposure counts the evidence ask custody DAA | NTU02 | NTU02 | NTU02 | CONFIRMED |
| TU-tkcov | KobCondAskKron | touch (update: arm on an ask, trail on a bid): the custody input tk carries the token covenant id | NTU02b | NTU02b | NTU02b | CONFIRMED |
| TU-owner | KobCondAskKron | touch (update: arm on an ask, trail on a bid): the custody input tk is owned by the evidence ask (owner = its covenant id, scheme covenant id) | NTU15, NTU16 | NTU15 NTU16 | NTU15 NTU16 | CONFIRMED |
| TU-tktpl | KobCondAskKron | touch (update: arm on an ask, trail on a bid): the custody input tk is a holder UTXO of the token's program (template hash; P2SH kept) | NTU02c | NTU02c(F) | NTU02c(F) | CONFIRMED |
| TU-tkp2sh | KobCondAskKron | touch (update: arm on an ask, trail on a bid): the custody input tk is the P2SH of the holder redeem script read (template hash kept) | NTU02d | NTU02d(F) | NTU02d(F) | CONFIRMED |
| TU-rule | KobCondAskKron | update, arm: the evidence ask quotes <= stopPrice | NTU19 | NTU19 | NTU19 | CONFIRMED |
| TT-k | KobCondAskKron | update, trail: the evidence bid justifies k >= 1 steps (rp >= stop + step + gap; k = 0 drains keeperTip and resets trailWait, k < 0 moves the stop down) | NTT19, NTT19b | NTT19 NTT19b | NTT19 NTT19b | CONFIRMED |
| TT-wait | KobCondAskKron | update, trail: at most once per trailWait DAA (CSV) | NT15 | NT15 | NT15 | CONFIRMED |
| TT-step | KobCondAskKron | update, trail: trailStep > 0 (a non-trailing order divides by trailStep = 0 and fails anyway) | stay rejected: NT19, NTU06 |  |  | held (defence in depth) |
| TU-armed | KobCondAskKron | update only while unarmed | NT14 | NT14 | NT14 | CONFIRMED |
| TU-own | KobCondAskKron | update spends no token input owned by the order (bounded scan; the evidence fill brings foreign token inputs) | NTU23, NTT23, NT13 | NTU23 NTT23 NT13 | NTU23 NTT23 NT13 | CONFIRMED |
| TI-push08 | KobIfdBidKron | touchBid (buy-stop entry): the evidence sigscript starts with the fixed 8-byte push of its fill argument (0x08) | NTI10, NTJ10, NTJ11 | NTI10 NTI11(F) NTJ10 NTJ11 | NTI10 NTI11(F) NTJ10 NTJ11 | CONFIRMED |
| TI-npos | KobIfdBidKron | touchBid (buy-stop entry): the evidence fill argument n > 0 | NTI11b, NTJ11b | NTI11b(F) NTJ11b(F) | NTI11b(F) NTJ11b(F) | CONFIRMED |
| TI-tpl | KobIfdBidKron | touchBid (buy-stop entry): the evidence is an instance of the KobAsk / KobBid template (template hash; P2SH kept) | NTI17, NTJ17 | NTI17 NTJ17 | NTI17 NTJ17 | CONFIRMED |
| TI-p2sh | KobIfdBidKron | touchBid (buy-stop entry): the evidence UTXO is the P2SH of the redeem script read (template hash kept) | NTI18, NTJ18 | NTI18 NTJ18 | NTI18 NTJ18 | CONFIRMED |
| TI-token | KobIfdBidKron | touchBid (buy-stop entry): the evidence trades this token (tokenCovId) | NTI07, NTJ07 | NTI07 NTJ07 | NTI07 NTJ07 | CONFIRMED |
| TI-scale | KobIfdBidKron | touchBid (buy-stop entry): the evidence quotes per the same scale (prices comparable) | NTI08, NTJ08 | NTI08 NTJ08 | NTI08 NTJ08 | CONFIRMED |
| TI-mintouch | KobIfdBidKron | touchBid (buy-stop entry): the evidence fill is >= minTouch base units | NTI09, NTJ09 | NTI09 NTJ09 | NTI09 NTJ09 | CONFIRMED |
| TI-slope | KobIfdBidKron | touchBid (buy-stop entry): the evidence does not decay (slope 0) | NTI05, NTJ05 | NTI05 NTJ05 | NTI05 NTJ05 | CONFIRMED |
| TI-cltv | KobIfdBidKron | touchBid (buy-stop entry): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTI01, NTI03, NTI04, NTJ01, NTJ03, NTJ04 | NTI01 NTI03 NTI04 NTJ01 NTJ03 NTJ04 | NTI01 NTI03 NTI04 NTJ01 NTJ03 NTJ04 | CONFIRMED |
| TI-interval | KobIfdBidKron | touchBid (buy-stop entry): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTI04, NTJ04 | NTI04 NTJ04 | NTI04 NTJ04 | CONFIRMED |
| TI-active | KobIfdBidKron | touchBid (buy-stop entry): exposure counts the evidence activeFrom | NTI03, NTJ03 | NTI03 NTJ03 | NTI03 NTJ03 | CONFIRMED |
| TI-auc | KobIfdBidKron | buy-stop entry armed inside its fill with an auction: the fill pays the stop (the auction opens at the trigger) | NI16b | NI16b | NI16b | CONFIRMED |
| TI-rule | KobIfdBidKron | fill, unarmed stop entry: the evidence bid quotes >= entryStop | NTI19 | NTI19 | NTI19 | CONFIRMED |
| TJ-rule | KobIfdBidKron | update: the evidence bid quotes >= entryStop | NTJ19 | NTJ19 | NTJ19 | CONFIRMED |
| TJ-amount | KobIfdBidKron | update: the entry has something to fill (an empty repeating entry would pay keeperTip for a useless arm) | NTJ24 | NTJ24 | NTJ24 | CONFIRMED |
| TJ-limit | KobIfdBidKron | update: the stop is on the limit's side (entryStop <= price; else no fill can follow the arm) | NTJ25 | NTJ25 | NTJ25 | CONFIRMED |
| TJ-origin | KobIfdBidKron | merge into an entry armed by update (armed 1, band): the continuation records the band origin (else the auction restarts) | NRP29 | NRP29 | NRP29 | CONFIRMED [positive scenarios rejected: RP11] |
| V3-A-minfill | KobAskKron | n >= minFill unless the fill takes everything left | V3A01 | V3A01 | V3A01 | CONFIRMED |
| V3-A-ceil | KobAskKron | proceeds rounded up (c = scale - 1 -> 0) | V3A02 | V3A02 | V3A02 | CONFIRMED |
| V3-A-tip | KobAskKron | p >= tip (also a decayed price): a negative proceeds rate would pay the maker nothing | V3A03, V3A04 | V3A03 V3A04 | V3A03 V3A04 | CONFIRMED |
| V3-B-minfill | KobBidKron | n >= minFill unless the bid ends (less than one minimum fill of buying power left) | V3B01 | V3B01 | V3B01 | CONFIRMED |
| V3-B-minfill0 | KobBidKron | minFill > 0 | V3B02 | V3B02 | V3B02 | CONFIRMED |
| V3-B-cont | KobBidKron | canContinue compares with the ceil budget of one minimum fill (c = scale - 1 -> 0) | V3B04 | V3B04 | V3B04 | CONFIRMED [positive scenarios rejected: V3B04+] |
| V3-B-floor | KobBidKron | the maker pays the floor (allIn c = 0 -> scale - 1) | V3B05 | V3B05 | V3B05 | CONFIRMED |
| V3-B-used | KobBidKron | the escrow is consumed at the ceil budget (used c = scale - 1 -> 0) | V3B06 | V3B06 | V3B06 | CONFIRMED [positive scenarios rejected: V3B05+] |
| V3-C-minfill | KobCondAskKron | n >= minFill unless the fill takes everything left | V3CA01 | V3CA01 | V3CA01 | CONFIRMED |
| V3-C-ceil | KobCondAskKron | proceeds rounded up (c = scale - 1 -> 0) | V3CA02 | V3CA02 | V3CA02 | CONFIRMED |
| V3-C-rpt | KobCondAskKron | re-arm budget returned to the entry on a sell-out rounded up (rptBudget c = scale - 1 -> 0; the entry's own floor, in + budget, does not count the exit carriers the continuation also gets, so the exit's check is the binding one) | V3CA03 | V3CA03 | V3CA03 | CONFIRMED [positive scenarios rejected: V3CA03+ V3IB03+] |
| V3-C-tip | KobCondAskKron | legPrice >= tip (either leg) | V3CA04, V3CA04b | V3CA04 V3CA04b | V3CA04 V3CA04b | CONFIRMED |
| V3-C-push | KobCondAskKron | the entry's sigscript starts with 0x08 (its merge argument is an 8-byte push; a fill of n >= 2^55 pushed by OP_PUSHDATA1 would alias a merge value; the KRON token refuses such amounts, so the exit input alone proves it) | V3CA06; stay rejected: NRP28 | V3CA06(F) | V3CA06(F) | CONFIRMED |
| V3-C-parent | KobCondAskKron | update never next to the repeat entry (OpCovInputCount(parent) == 0): an 8-byte ev push would alias the merge amount | V3CA05; stay rejected: NRP22 | V3CA05 | V3CA05 | CONFIRMED |
| V3-C-mergek | KobCondAskKron | re-arm: the exit sells n < 2^53 (the merge argument -(self * 2^53 + n) names this input and n only then: an exit of 2^53 + m at input i reads the merge of the booked exit at input i + 1 as its own; the KRON token refuses custodies above 10^9, so the exit input alone proves it) | V3CA09 | V3CA09(F) | V3CA09(F) | CONFIRMED |
| V3-I-floor | KobIfdBidKron | the maker pays the floor (spend c = 0 -> scale - 1) | V3IB02 | V3IB02 | V3IB02 | CONFIRMED |
| V3-I-mergeceil | KobIfdBidKron | merge floor: the budget of m comes back rounded up (c = scale - 1 -> 0) | V3IB03 | V3IB03 | V3IB03 | CONFIRMED |
| V3-I-push | KobIfdBidKron | the exit's sigscript starts with 0x08 (its settle n is an 8-byte push; a refund pushed by OP_PUSHDATA1 would read as m = 8) | V3IB04; stay rejected: NRP22 | V3IB04 | V3IB04 | CONFIRMED |
| V3-I-mergek | KobIfdBidKron | a booked exit's amount is below MERGE_K (2^53: it must fit the merge argument; the KRON token refuses amounts above 10^9, so the entry input alone proves it) | V3IB06 | V3IB06(F) | V3IB06(F) | CONFIRMED |

Collateral flips (scenarios accepted besides the expected ones):

- A5: NV10
- B7: KS6b
- C6: NA4
- C16: NRP19
- C16b: NRP14 NRP19
- C17: NRP14
- I1: NI14 NI15 NI15b
- R5: NRP3 NRP3b NRP29
- FXL3b: NRP20
- TI-push08: NTI11

## kron-buy

KRON adapter (v3, buy-side contracts); test binary `kob_kron_v2_buy_tests`; contracts `contracts/adapters/kron/v2`. 110/110 confirmed, 4/4 defence-in-depth held (expected NOT to flip; IA24 after the NRPB2 fix, re-run alone).
Baseline (no mutation): kron_v2_buy_touch_cond_arm clean, kron_v2_buy_touch_cond_trail clean, kron_v2_buy_trail_cap_attack clean, kron_v2_buy_lifecycle clean, kron_v2_buy_v3_min_fill clean, kron_v2_buy_v3_rounding clean, kron_v2_buy_v3_merge_push clean, kron_v2_buy_lifecycle_dust_custody clean, kron_v2_buy_negative clean, kron_v2_buy_repeat_ifd_attacks clean, kron_v2_buy_v3_tip clean, kron_v2_buy_touch_ifd_arm clean, kron_empty_repeating_ifda_refund_is_refused clean, kron_v2_buy_touch_cond_settle clean, kron_v2_buy_touch_ifd_fill clean.

| id | contract | check ablated | expected | flipped 2433 | flipped 2732 | status |
|---|---|---|---|---|---|---|
| CB1 | KobCondBidKron | settle: noStrays(selfId) call | NS10 | NS10 | NS10 | CONFIRMED |
| CB1b | KobCondBidKron | noStrays: scan bound require(cnt <= MAX_TOK_IN) (the scan is unrolled over MAX_TOK_IN slots: a further input is not scanned) | KB-P3b | KB-P3b(F) | KB-P3b(F) | CONFIRMED |
| CB2 | KobCondBidKron | refund: require(OpCovInputCount(tokenCovId) == 0) | NS11 | NS11 | NS11 | CONFIRMED |
| CB3 | KobCondBidKron | update refuses token inputs owned by the order (touch: the evidence fill brings foreign token inputs) | NS12, NTV23, NTW23 | NS12 NTV23 NTW23 | NS12 NTV23 NTW23 | CONFIRMED |
| CB4 | KobCondBidKron | stop auction: require(t >= newArmed) (origin bound) | NB29 | NB29 | NB29 | CONFIRMED |
| CB5 | KobCondBidKron | settle: continuation value >= keep | NB27, NB1, NB4 | NB27 NB1 NB4 | NB27 NB1 NB4 | CONFIRMED |
| CB6 | KobCondBidKron | settle: continuation script == contSpk (auction origin carried, no restart) | NB28, NB15, NB16 | NB28 NB15 NB16 | NB28 NB15 NB16 | CONFIRMED |
| CB7 | KobCondBidKron | update: keeper takes at most keeperTip (the continuation keeps at least in - keeperTip, floorOut); NB30 / NB25 (an escrow of exactly the budget) stay refused by the budget floor (need), NB30c (a tip funded beyond the budget) is refused by this bound only | NB30c; stay rejected: NB30, NB25 | NB30c | NB30c | CONFIRMED |
| CB8 | KobCondBidKron | trail: cap step count below the limit leg (k = min(k, kf)) | NB22b | NB22b | NB22b | CONFIRMED |
| CB9 | KobCondBidKron | repeat: stop-leg fill refuses the entry (require(present == 0)) | NRPB13 | NRPB13 | NRPB13 | CONFIRMED |
| CB10 | KobCondBidKron | repeat: TP without the entry only from rptUntil | NRPB10 | NRPB10 | NRPB10 | CONFIRMED |
| CB11 | KobCondBidKron | repeat: entry merge argument names this exit and n (NRPB34: an arming update of the entry, its ev pushed as 0x08 \|\| ev, is no merge) | NRPB14, NRPB34 | NRPB14 NRPB34 | NRPB14 NRPB34 | CONFIRMED |
| CB12 | KobCondBidKron | repeat: maker profit >= proceeds - spend | NRPB11 | NRPB11 | NRPB11 | CONFIRMED |
| CB13 | KobCondBidKron | repeat: continuation keep == in - proceeds - ceil(prefund of n) | NRPB12 | NRPB12 | NRPB12 | CONFIRMED |
| CB14 | KobCondBidKron | update: continuation script == contSpk (exact step count, no restart) | NB21, NB21b | NB21 NB21b | NB21 NB21b | CONFIRMED |
| CB15 | KobCondBidKron | trail: at most once per trailWait (require(this.ageDaa >= trailWait)) | NB20 | NB20 | NB20 | CONFIRMED |
| CB-minfill | KobCondBidKron | v3: minimum fill (n >= minFill unless n == amountLeft) | NV3CB01 | NV3CB01 | NV3CB01 | CONFIRMED |
| CB-spendround | KobCondBidKron | v3: the buy spend is rounded DOWN (the maker pays at most the floor); ceil would let the filler keep one sompi more | NV3CB10 | NV3CB10 | NV3CB10 | CONFIRMED |
| CB-updateparent | KobCondBidKron | v3: an exit never arms/trails next to its repeat entry (require(OpCovInputCount(parent) == 0)) | NV3CB41 | NV3CB41 | NV3CB41 | CONFIRMED |
| CB-pin08 | KobCondBidKron | v3: the repeat entry pin starts with the 8-byte merge push (0x08); a 9-byte push whose low 8 bytes alias the merge argument passes the value check but not this one | NV3CB42 | NV3CB42(F) | NV3CB42(F) | CONFIRMED |
| CB-rearm-mergek | KobCondBidKron | v3: re-arm: n < MERGE_K (2^53), so the merge argument -(self * 2^53 + n) names this input and n (a TP fill of 2^53 + m would read as the merge of the booked exit at self + 1; the re-arm moves no tokens of its own, so the KRON amount cap does not stop it) | NV3CB43 | NV3CB43 | NV3CB43 | CONFIRMED |
| IA1 | KobIfdAskKron | settle: custody holds exactly amountLeft base units (amount check) | NS15b, NRPB25 | NS15b NRPB25 | NS15b NRPB25 | CONFIRMED |
| IA2 | KobIfdAskKron | settle: noStrays(selfId, custody) call | NS13, NS14, NRPB24, NRPB24b | NS13 NS14 NRPB24 NRPB24b | NS13 NS14 NRPB24 NRPB24b | CONFIRMED |
| IA2b | KobIfdAskKron | noStrays: scan bound require(cnt <= MAX_TOK_IN) | KI-P3b | KI-P3b(F) | KI-P3b(F) | CONFIRMED |
| IA3 | KobIfdAskKron | update refuses token inputs owned by the entry (touch: the evidence fill brings foreign token inputs) | NI22, NTL23 | NI22 NTL23 | NI22 NTL23 | CONFIRMED |
| IA4 | KobIfdAskKron | close: require(OpCovInputCount(tokenCovId) == 0) | NRPB28 | NRPB28 | NRPB28 | CONFIRMED |
| IA5 | KobIfdAskKron | close: require(amountLeft == 0) | NRPB26 | NRPB26 | NRPB26 | CONFIRMED |
| IA6 | KobIfdAskKron | close: not before soft expiry / idle bound | NRPB27 | NRPB27 | NRPB27 | CONFIRMED |
| IA7 | KobIfdAskKron | update: keeper takes at most keeperTip | NI23 | NI23 | NI23 | CONFIRMED |
| IA8 | KobIfdAskKron | stop entry: unarmed fill reads its trigger evidence (touch) | NI15, NI16, NI17 | NI15 NI16 NI17 | NI15 NI16 NI17 | CONFIRMED |
| IA8c | KobIfdAskKron | settle, unarmed stop entry: the evidence ask quotes <= entryStop | NI17, NTK19 | NI17 NTK19 | NI17 NTK19 | CONFIRMED |
| TK-auc | KobIfdAskKron | sell-stop entry armed inside its fill with an auction: the fill sells at the stop (the auction opens at the trigger) | NI18b | NI18b | NI18b | CONFIRMED |
| IA9 | KobIfdAskKron | stop entry: require(entryStop >= price) | NI21 | NI21 | NI21 | CONFIRMED |
| IA10 | KobIfdAskKron | minFill: require(n >= minFill \|\| outAmount == 0) | NI20 | NI20 | NI20 | CONFIRMED |
| IA11 | KobIfdAskKron | exit funded with proceeds + prefund + carrier (auction price paid) | NI18, NI6 | NI18b NI18 NI6 | NI18b NI18 NI6 | CONFIRMED |
| IA12 | KobIfdAskKron | continuation script == contSpk (auction origin, cycle count, amount, unarmed re-arm) | NI19, NRPB3, NRPB19, NRPB19b, NRPB19c, NRPB19d | NI19 NRPB3 NRPB19 NRPB19b NRPB19c NRPB19d NRPB35 | NI19 NRPB3 NRPB19 NRPB19b NRPB19c NRPB19d NRPB35 | CONFIRMED |
| IA13 | KobIfdAskKron | booking: the exit's rptUntil counts from the entry UTXO's DAA (its last activity), never from the filler's t (the cycle dated by t instead) | NRPB6 | NRPB6 | NRPB6 | CONFIRMED |
| IA14 | KobIfdAskKron | booking: rptAmount decremented by n | NRPB3 | NRPB3 | NRPB3 | CONFIRMED |
| IA15 | KobIfdAskKron | repeating entry never terminates on a fill (outAmount > 0 \|\| rptAmount > 0) | NRPB7 | NRPB7 | NRPB7 | CONFIRMED |
| IA16 | KobIfdAskKron | sold-out repeating entry keeps the custody carrier (floor + tokenIn value) | NRPB8 | NRPB8 | NRPB8 | CONFIRMED |
| IA17 | KobIfdAskKron | merge: exit parent == this entry (part of the exit-tail comparison with rate; a plain exit also fails on rate, so removing the parent alone holds) | stay rejected: NRPB20 |  |  | held (defence in depth) |
| IA18 | KobIfdAskKron | merge: input k is a genuine KobCondBidKron (template + P2SH check) (the crafted input also fails the exit-terms comparison, so removing the template check alone holds) | stay rejected: NRPB21 |  |  | held (defence in depth) |
| IA19 | KobIfdAskKron | merge: partial buy-back, entry re-armed with ceil(prefund of m) | NRPB15 | NRPB15 NRPB31 | NRPB15 NRPB31 | CONFIRMED |
| IA20 | KobIfdAskKron | merge: sell-out leftovers come back (floor with the exit value) | NRPB16 | NRPB16 NRPB31 | NRPB16 NRPB31 | CONFIRMED |
| IA21 | KobIfdAskKron | merge: custody carrier kept (tokOut value >= keepCarrier) | NRPB16b, NRPB16c | NRPB16b NRPB16c | NRPB16b NRPB16c | CONFIRMED |
| IA22 | KobIfdAskKron | token output pin (custody / remainder / refund state: owner, amount, type) | NRPB17, NRPB18 | NRPB17 NRPB18 | NRPB17 NRPB18 | CONFIRMED |
| IA23 | KobIfdAskKron | merge into an empty stop entry re-arms unarmed (newArmed = 0) | NRPB19d | NRPB19d | NRPB19d | CONFIRMED |
| IA24 | KobIfdAskKron | booking, both binds of the exit removed: exit script equality AND exit genesis id (NI4 is rejected by the covenant rules independently of both) | NRPB1, NRPB2, NRPB4, NRPB4b, NRPB5, NI1, NI2, NI2b, NI3, NI3b, NI5 | NRPB1 NRPB2 NRPB4 NRPB4b NRPB5 NRPB6 NI1 NI2 NI2b NI3 NI3b NI5 | NRPB1 NRPB2 NRPB4 NRPB4b NRPB5 NRPB6 NI1 NI2 NI2b NI3 NI3b NI5 | CONFIRMED |
| IA24a | KobIfdAskKron | ONLY the exit script equality removed: the genesis id binds the exit script too (defence in depth) | stay rejected: NRPB1, NRPB4, NRPB5, NI1, NI2, NI2b |  |  | held (defence in depth) |
| IA24b | KobIfdAskKron | ONLY the exit genesis id removed: double / aliased / attacker-authorised exits are accepted; the script equality still binds the tampered / plain / altered exits | NI3, NI3b, NI5; stay rejected: NI1, NI2, NI2b, NRPB1, NRPB4, NRPB5 | NI3 NI3b NI5 | NI3 NI3b NI5 | CONFIRMED |
| IA-minfill | KobIfdAskKron | v3: minimum fill (n >= minFill unless the fill sells out) | NV3IA01 | NV3IA01 | NV3IA01 | CONFIRMED |
| IA-procround | KobIfdAskKron | v3: the exit proceeds are rounded UP (ceil, the maker receives at least the exact value) | NV3IA10 | NV3IA10 | NV3IA10 | CONFIRMED |
| IA-preround | KobIfdAskKron | v3: the exit prefund is rounded UP (ceil) | NV3IA11 | NV3IA11 | NV3IA11 | CONFIRMED [positive scenarios rejected: V3IA11] |
| IA-mergeround | KobIfdAskKron | v3: a re-arming buy-back returns at least ceil(prefund of m) to the entry | NV3IA12 | NV3IA12 | NV3IA12 | CONFIRMED |
| IA-pricetip | KobIfdAskKron | v3: a sell entry requires price >= tip (so p >= price >= tip; a booked rptPrice = price - tip is never negative) | NV3IA21 | NV3IA21 | NV3IA21 | CONFIRMED |
| IA-k08 | KobIfdAskKron | v3: the booked exit at k starts with the 8-byte fill push (0x08); a 9-byte push whose low 8 bytes alias m passes the value check and the exit-terms comparison but not this one | NV3IA40; stay rejected: NRPB32 | NV3IA40(F) | NV3IA40(F) | CONFIRMED |
| IA-merge-scale | KobIfdAskKron | v3: the merge requires the committed exit to quote at the entry's scale (exitState [118..126); an exit at scale 999 refused: the two would round the merge differently) | NRPB36 | NRPB36 | NRPB36 | CONFIRMED |
| FXL3c | KobIfdAskKron | the merge requires the exit terms [0..190) (maker, legs) to be the committed exitState | NRPB29 | NRPB29 | NRPB29 | CONFIRMED |
| FXL3d | KobIfdAskKron | the merge requires parent = this entry, rptPrice and rptPre = its own (exit tail [288..340)) | NRPB30 | NRPB20 NRPB30 | NRPB20 NRPB30 | CONFIRMED |
| FXL3e | KobIfdAskKron | the merge requires the exit to run its settle with n = m (sigscript [1..9)); NRPB33: an arming update of the exit whose ev is pushed as 0x08 \|\| ev is no settle | NRPB33; stay rejected: NRPB32 | NRPB33(F) | NRPB33(F) | CONFIRMED |
| FXL3f | KobIfdAskKron | a sell-out merge never costs the entry (floor = max(prefund, exit value - proceeds)) | NRPB31 | NRPB31 | NRPB31 | CONFIRMED |
| TB-push08 | KobCondBidKron | touchBid (settle, buy-stop leg): the evidence sigscript starts with the fixed 8-byte push of its fill argument (0x08) | NTB10 | NTB10 NTB11(F) | NTB10 NTB11(F) | CONFIRMED |
| TB-npos | KobCondBidKron | touchBid (settle, buy-stop leg): the evidence fill argument n > 0 | NTB11b | NTB11b(F) | NTB11b(F) | CONFIRMED |
| TB-tpl | KobCondBidKron | touchBid (settle, buy-stop leg): the evidence is an instance of the KobAsk / KobBid template (template hash; P2SH kept) | NTB17 | NTB17 | NTB17 | CONFIRMED |
| TB-p2sh | KobCondBidKron | touchBid (settle, buy-stop leg): the evidence UTXO is the P2SH of the redeem script read (template hash kept) | NTB18 | NTB18 | NTB18 | CONFIRMED |
| TB-token | KobCondBidKron | touchBid (settle, buy-stop leg): the evidence trades this token (tokenCovId) | NTB07 | NTB07 | NTB07 | CONFIRMED |
| TB-scale | KobCondBidKron | touchBid (settle, buy-stop leg): the evidence quotes per the same scale (prices comparable) | NTB08 | NTB08 | NTB08 | CONFIRMED |
| TB-mintouch | KobCondBidKron | touchBid (settle, buy-stop leg): the evidence fill is >= minTouch base units | NTB09 | NTB09 | NTB09 | CONFIRMED |
| TB-slope | KobCondBidKron | touchBid (settle, buy-stop leg): the evidence does not decay (slope 0) | NTB05 | NTB05 | NTB05 | CONFIRMED |
| TB-cltv | KobCondBidKron | touchBid (settle, buy-stop leg): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTB01, NTB03, NTB04 | NTB01 NTB03 NTB04 | NTB01 NTB03 NTB04 | CONFIRMED |
| TB-interval | KobCondBidKron | touchBid (settle, buy-stop leg): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTB04 | NTB04 | NTB04 | CONFIRMED |
| TB-active | KobCondBidKron | touchBid (settle, buy-stop leg): exposure counts the evidence activeFrom | NTB03 | NTB03 | NTB03 | CONFIRMED |
| TB-rule | KobCondBidKron | settle, unarmed buy-stop leg: the evidence bid quotes >= stopPrice | NTB19, NB13 | NTB19 NB13 | NTB19 NB13 | CONFIRMED |
| TV-push08 | KobCondBidKron | touch (update: arm on a bid, trail on an ask): the evidence sigscript starts with the fixed 8-byte push of its fill argument (0x08) | NTV10, NTV11, NTW10 | NTV10 NTV11 NTW10 | NTV10 NTV11 NTW10 | CONFIRMED |
| TV-npos | KobCondBidKron | touch (update: arm on a bid, trail on an ask): the evidence fill argument n > 0 | NTW11 | NTW11 | NTW11 | CONFIRMED |
| TV-tpl | KobCondBidKron | touch (update: arm on a bid, trail on an ask): the evidence is an instance of the KobAsk / KobBid template (template hash; P2SH kept) | NTV17, NTW17 | NTV17 NTW17 | NTV17 NTW17 | CONFIRMED |
| TV-p2sh | KobCondBidKron | touch (update: arm on a bid, trail on an ask): the evidence UTXO is the P2SH of the redeem script read (template hash kept) | NTV18, NTW18 | NTV18 NTW18 | NTV18 NTW18 | CONFIRMED |
| TV-token | KobCondBidKron | touch (update: arm on a bid, trail on an ask): the evidence trades this token (tokenCovId) | NTV07, NTW07b | NTV07 NTW07b | NTV07 NTW07b | CONFIRMED |
| TV-scale | KobCondBidKron | touch (update: arm on a bid, trail on an ask): the evidence quotes per the same scale (prices comparable) | NTV08, NTW08 | NTV08 NTW08 | NTV08 NTW08 | CONFIRMED |
| TV-mintouch | KobCondBidKron | touch (update: arm on a bid, trail on an ask): the evidence fill is >= minTouch base units | NTV09, NTW09 | NTV09 NTW09 | NTV09 NTW09 | CONFIRMED |
| TV-slope | KobCondBidKron | touch (update: arm on a bid, trail on an ask): the evidence does not decay (slope 0) | NTV05, NTW05 | NTV05 NTW05 | NTV05 NTW05 | CONFIRMED |
| TV-cltv | KobCondBidKron | touch (update: arm on a bid, trail on an ask): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTV01, NTV03, NTV04, NTW01, NTW02, NTW03, NTW04 | NTV01 NTV03 NTV04 NTW01 NTW02 NTW03 NTW04 | NTV01 NTV03 NTV04 NTW01 NTW02 NTW03 NTW04 | CONFIRMED |
| TV-interval | KobCondBidKron | touch (update: arm on a bid, trail on an ask): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTV04, NTW04 | NTV04 NTW04 | NTV04 NTW04 | CONFIRMED |
| TV-active | KobCondBidKron | touch (update: arm on a bid, trail on an ask): exposure counts the evidence activeFrom | NTV03, NTW03 | NTV03 NTW03 | NTV03 NTW03 | CONFIRMED |
| TV-tokdaa | KobCondBidKron | touch (update: arm on a bid, trail on an ask): exposure counts the evidence ask custody DAA | NTW02 | NTW02 | NTW02 | CONFIRMED |
| TV-tkcov | KobCondBidKron | touch (update: arm on a bid, trail on an ask): the custody input tk carries the token covenant id | NTW02b | NTW02b | NTW02b | CONFIRMED |
| TV-owner | KobCondBidKron | touch (update: arm on a bid, trail on an ask): the custody input tk is owned by the evidence ask (owner = its covenant id, scheme covenant id) | NTW15, NTW16 | NTW15 NTW16 | NTW15 NTW16 | CONFIRMED |
| TV-tktpl | KobCondBidKron | touch (update: arm on a bid, trail on an ask): the custody input tk is a holder UTXO of the token's program (template hash; P2SH kept) | NTW02c | NTW02c(F) | NTW02c(F) | CONFIRMED |
| TV-tkp2sh | KobCondBidKron | touch (update: arm on a bid, trail on an ask): the custody input tk is the P2SH of the holder redeem script read (template hash kept) | NTW02d | NTW02d(F) | NTW02d(F) | CONFIRMED |
| TV-rule | KobCondBidKron | update, arm: the evidence bid quotes >= stopPrice | NTV19 | NTV19 | NTV19 | CONFIRMED |
| TW-k | KobCondBidKron | update, trail: the evidence ask justifies k >= 1 steps (k = 0 drains keeperTip and resets trailWait, k < 0 moves the stop up) | NTW19, NTW19b | NTW19 NTW19b | NTW19 NTW19b | CONFIRMED |
| TW-step | KobCondBidKron | update, trail: trailStep > 0 (a non-trailing order divides by trailStep = 0 and fails anyway) | stay rejected: NB24, NTV06 |  |  | held (defence in depth) |
| TV-armed | KobCondBidKron | update only while unarmed | NB19 | NB19 | NB19 | CONFIRMED |
| TK-push08 | KobIfdAskKron | touchAsk (sell-stop entry): the evidence sigscript starts with the fixed 8-byte push of its fill argument (0x08) | NTK10, NTL10 | NTK10 NTL10 | NTK10 NTL10 | CONFIRMED |
| TK-npos | KobIfdAskKron | touchAsk (sell-stop entry): the evidence fill argument n > 0 | NTK11, NTL11 | NTK11 NTL11 | NTK11 NTL11 | CONFIRMED |
| TK-tpl | KobIfdAskKron | touchAsk (sell-stop entry): the evidence is an instance of the KobAsk / KobBid template (template hash; P2SH kept) | NTK17, NTL17 | NTK17 NTL17 | NTK17 NTL17 | CONFIRMED |
| TK-p2sh | KobIfdAskKron | touchAsk (sell-stop entry): the evidence UTXO is the P2SH of the redeem script read (template hash kept) | NTK18, NTL18 | NTK18 NTL18 | NTK18 NTL18 | CONFIRMED |
| TK-token | KobIfdAskKron | touchAsk (sell-stop entry): the evidence trades this token (tokenCovId) | NTK07b, NTL07b | NTK07b NTL07b | NTK07b NTL07b | CONFIRMED |
| TK-scale | KobIfdAskKron | touchAsk (sell-stop entry): the evidence quotes per the same scale (prices comparable) | NTK08, NTL08 | NTK08 NTL08 | NTK08 NTL08 | CONFIRMED |
| TK-mintouch | KobIfdAskKron | touchAsk (sell-stop entry): the evidence fill is >= minTouch base units | NTK09, NTL09 | NTK09 NTL09 | NTK09 NTL09 | CONFIRMED |
| TK-slope | KobIfdAskKron | touchAsk (sell-stop entry): the evidence does not decay (slope 0) | NTK05, NTL05 | NTK05 NTL05 | NTK05 NTL05 | CONFIRMED |
| TK-cltv | KobIfdAskKron | touchAsk (sell-stop entry): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTK01, NTK02, NTK03, NTK04, NTL01, NTL02, NTL03, NTL04 | NTK01 NTK02 NTK03 NTK04 NTL01 NTL02 NTL03 NTL04 | NTK01 NTK02 NTK03 NTK04 NTL01 NTL02 NTL03 NTL04 | CONFIRMED |
| TK-interval | KobIfdAskKron | touchAsk (sell-stop entry): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTK04, NTL04 | NTK04 NTL04 | NTK04 NTL04 | CONFIRMED |
| TK-active | KobIfdAskKron | touchAsk (sell-stop entry): exposure counts the evidence activeFrom | NTK03, NTL03 | NTK03 NTL03 | NTK03 NTL03 | CONFIRMED |
| TK-tokdaa | KobIfdAskKron | touchAsk (sell-stop entry): exposure counts the evidence ask custody DAA | NTK02, NTL02 | NTK02 NTL02 | NTK02 NTL02 | CONFIRMED |
| TK-tkcov | KobIfdAskKron | touchAsk (sell-stop entry): the custody input tk carries the token covenant id | NTK02b, NTL02b | NTK02b NTL02b | NTK02b NTL02b | CONFIRMED |
| TK-owner | KobIfdAskKron | touchAsk (sell-stop entry): the custody input tk is owned by the evidence ask (owner = its covenant id, scheme covenant id) | NTK15, NTK16, NTL15, NTL16 | NTK15 NTK16 NTL15 NTL16 | NTK15 NTK16 NTL15 NTL16 | CONFIRMED |
| TK-tktpl | KobIfdAskKron | touchAsk (sell-stop entry): the custody input tk is a holder UTXO of the token's program (template hash; P2SH kept) | NTK02c, NTL02c | NTK02c(F) NTL02c(F) | NTK02c(F) NTL02c(F) | CONFIRMED |
| TK-tkp2sh | KobIfdAskKron | touchAsk (sell-stop entry): the custody input tk is the P2SH of the holder redeem script read (template hash kept) | NTK02d, NTL02d | NTK02d(F) NTL02d(F) | NTK02d(F) NTL02d(F) | CONFIRMED |
| TL-rule | KobIfdAskKron | update: the evidence ask quotes <= entryStop | NTL19 | NTL19 | NTL19 | CONFIRMED |
| TL-amount | KobIfdAskKron | update: the entry has base units to fill (an empty repeating entry would pay keeperTip for a useless arm) | NTL24 | NTL24 | NTL24 | CONFIRMED |
| TL-limit | KobIfdAskKron | update: the stop is on the limit's side (entryStop >= price; else no fill can follow the arm) | NTL25 | NTL25 | NTL25 | CONFIRMED |
| TL-origin | KobIfdAskKron | merge into an entry armed by update (armed 1, band): the continuation records the band origin (else the auction restarts) | NRPB35 | NRPB35 | NRPB35 | CONFIRMED [positive scenarios rejected: RPB10] |
| IA-refund-held | KobIfdAskKron | refund (settle nb = 0) only while tokens are held: an empty repeating entry with a zero-amount custody would pin no output, so close() is its end (inputOnly: whether the KRON program itself takes the zero-amount custody is not what this check decides) | SR2 | SR2(F) | SR2(F) | CONFIRMED |

Collateral flips (scenarios accepted besides the expected ones):

- IA11: NI18b
- IA12: NRPB35
- IA19: NRPB31
- IA20: NRPB31
- IA24: NRPB6
- FXL3d: NRPB20
- TB-push08: NTB11

## pair

pair orders: KobPair; test binary `kob_pair_tests`; contracts `contracts/v2`. 82/82 confirmed, 2/2 defence-in-depth held (expected NOT to flip).
Baseline (no mutation): pair_aliasing clean, pair_custody clean, pair_params clean, pair_settlement clean, pair_strays clean, pair_lifecycle clean, pair_fake_quotes clean, kob_cond_pair_tests::cond_pair_evidence_fake_quotes clean, pair_carriers clean, pair_outputs clean.

| id | contract | check ablated | expected | flipped | status |
|---|---|---|---|---|---|
| S1 | KobPair | the order is the only input of its covenant id (a sibling UTXO of the id) | NP45a, NP45b | NP45a NP45b | CONFIRMED |
| C0 | KobPair | the custody input carries the S covenant (not a UTXO of another token owned by the order) | NP24a, NP24b | NP24a NP24b | CONFIRMED |
| C1 | KobPair | the custody input is a P2SH spend of its redeem script (a planted UTXO of the S covenant id) | NP25a, NP25b | NP25a NP25b | CONFIRMED |
| C2 | KobPair | the custody's template is S's (a planted P2SH UTXO of a look-alike template) | NP26a, NP26b | NP26a NP26b | CONFIRMED |
| C3r | KobPair | KRON custody owned by this order (two orders naming one custody) | NP20sr | NP20sr | CONFIRMED |
| C3k | KobPair | KCC-20 custody owned by this order (two orders naming one custody) | NP20sk | NP20sk | CONFIRMED |
| C4r | KobPair | KRON custody of owner type 2 (covenant id; id_type 3 is also refused by the token program) | NP21sr | NP21sr(F) | CONFIRMED |
| C5r | KobPair | KRON custody not a minter | NP22sr | NP22sr | CONFIRMED |
| C4k | KobPair | KCC-20 custody of owner scheme 0x04 with borrowing disabled | NP21sk | NP21sk | CONFIRMED |
| C6k | KobPair | sFamily is 1 or 2 (hostile field read as KCC-20) | NP22sk | NP22sk | CONFIRMED |
| C7 | KobPair | a positive custody: a zero custody leaves output i of a refund unpinned (a KRON program also refuses the empty token output) | NP27a, NP27b | NP27a NP27b(F) NP27a(F) NP27b | CONFIRMED |
| C9s | KobPair | sScale > 0 (hostile field: an ask at a negative scale of A sells for one unit of B) | NP64a | NP64a | CONFIRMED |
| C9t | KobPair | tScale > 0 (hostile field: a bid at a negative scale of A pays a negative quote) | NP64b | NP64b | CONFIRMED |
| C8 | KobPair | exact custody: held == custody (an extra unit to the matcher) | NP06, NP07 | NP06 NP07 | CONFIRMED |
| G1 | KobPair | no S input owned by this id but the custody (rest, IOC, close, refund) | NP30a0, NP30a1, NP30a2, NP30b0, NP30b1, NP30b2, NP32a, NP32b | NP30a0 NP30a1 NP30a2 NP32a NP30b0 NP30b1 NP30b2 NP32b | CONFIRMED |
| G2 | KobPair | no T input owned by this id in a fill (rest, IOC, close) | NP31a0, NP31a1, NP31a2, NP31b0, NP31b1, NP31b2 | NP31a0 NP31a1 NP31a2 NP31b0 NP31b1 NP31b2 NP34t0 NP34t1 NP34t2 NP34t3 NP34t4 NP34t5 NP34t6 NP34t7 NP35t(F) | CONFIRMED |
| GS0 | KobPair | noStrays scan slot 0: a stray of the order at the input of slot 0 of its token | NP34t0 | NP34t0 | CONFIRMED |
| GS1 | KobPair | noStrays scan slot 1: a stray of the order at the input of slot 1 of its token | NP34t1 | NP30a0 NP31a0 NP30a1 NP31a1 NP30a2 NP31a2 NP32a NP30b0 NP31b0 NP30b1 NP31b1 NP30b2 NP31b2 NP32b NP34t1 | CONFIRMED |
| GS2 | KobPair | noStrays scan slot 2: a stray of the order at the input of slot 2 of its token | NP34t2 | NP34t2 | CONFIRMED |
| GS3 | KobPair | noStrays scan slot 3: a stray of the order at the input of slot 3 of its token | NP34t3 | NP34t3 | CONFIRMED |
| GS4 | KobPair | noStrays scan slot 4: a stray of the order at the input of slot 4 of its token | NP34t4 | NP34t4 | CONFIRMED |
| GS5 | KobPair | noStrays scan slot 5: a stray of the order at the input of slot 5 of its token | NP34t5 | NP34t5 | CONFIRMED |
| GS6 | KobPair | noStrays scan slot 6: a stray of the order at the input of slot 6 of its token | NP34t6 | NP34t6 | CONFIRMED |
| GS7 | KobPair | noStrays scan slot 7: a stray of the order at the input of slot 7 of its token | NP34t7 | NP34t7 | CONFIRMED |
| GB | KobPair | noStrays bound: at most MAX_TOK_IN = 8 inputs of the token (a 9th is not scanned; the KCC-20 program refuses it on its own input) | NP35t | NP35t(F) | CONFIRMED |
| G3 | KobPair | a refund carries no T input (T strays stay put) | NP33a, NP33b | NP33a NP33b | CONFIRMED |
| E1 | KobPair | refund only once due (expiry, 90 days idle, IOC / FOK kill) | NP70a, NP70b, NP71a, NP71b, NP72a, NP72b, NP73a, NP73b | NP70a NP71a NP72a NP73a NP70b NP71b NP72b NP73b | CONFIRMED |
| E1a | KobPair | refund from expiryDaa, not one DAA earlier | NP70a, NP70b | NP70a NP70b | CONFIRMED |
| E1b | KobPair | the idle bound is 90 days of DAA | NP71a, NP71b | NP71a NP71b | CONFIRMED |
| E1c | KobPair | the IOC / FOK kill is 600 DAA | NP72a, NP72b | NP72a NP73a NP72b NP73b | CONFIRMED |
| E1d | KobPair | the kill counts from max(UTXO DAA, activeFrom) | NP73a, NP73b | NP73a NP73b | CONFIRMED |
| E3 | KobPair | a refund leaves no output bound to the order id (a forged owner of its strays) | NP77a, NP77b | NP77a NP77b | CONFIRMED |
| E4 | KobPair | a refund pays the maker everything but refundTip | NP74a, NP74b | NP74a NP74b | CONFIRMED |
| N1 | KobPair | n <= amountLeft (a bid overfilled from escrow slack; an ask is held by its custody) | NP08; stay rejected: NP09 | NP08 | CONFIRMED |
| N2 | KobPair | n > 0: a negative n is refused by sOut == n / tOut == n and the token programs (no negative amounts) | stay rejected: NP55 |  | held (defence in depth) |
| N3 | KobPair | minimum fill | NP50 | NP50 | CONFIRMED |
| N4 | KobPair | no fill before activeFrom | NP54 | NP54 | CONFIRMED |
| N5 | KobPair | tip >= 0 (hostile field) | NP61 | NP61 | CONFIRMED |
| N6 | KobPair | deliveryCarrier >= 0 (hostile field) | NP62 | NP62 | CONFIRMED |
| N7 | KobPair | TWAP / DCA interval (CSV) | NP52 | NP52 | CONFIRMED |
| N8 | KobPair | maxFill | NP51 | NP51 | CONFIRMED |
| N9 | KobPair | an ask releases exactly n | NP02 | NP02 | CONFIRMED |
| D1 | KobPair | decay slope > 0 (hostile field) | NP58 | NP58 | CONFIRMED |
| D2 | KobPair | decayStep > 0 (hostile field; 0 also fails the division) | NP59 | NP59 | CONFIRMED |
| D3 | KobPair | decay time t proven by CLTV (an overstated t: a lower price) | NP56 | NP56 | CONFIRMED |
| D4 | KobPair | decay time t not before the origin | NP57 | NP57 | CONFIRMED |
| D5 | KobPair | the decay origin of a TWAP slice is its opening (UTXO DAA + interval) | NP65 | NP65 | CONFIRMED |
| D6 | KobPair | a Dutch ask stops at priceEnd | NP66 | NP66 | CONFIRMED |
| D7 | KobPair | a rising bid stops at priceEnd | NP67 | NP67 | CONFIRMED |
| D8 | KobPair | the quote is positive (a bid at 0 takes A for nothing) | NP60 | NP60 | CONFIRMED |
| D9 | KobPair | side is 1 or 2 (hostile field) | NP63 | NP63 | CONFIRMED |
| Q1 | KobPair | an ask receives at least the ceil | NP01 | NP01 | CONFIRMED |
| Q1r | KobPair | an ask's quote is rounded up (the floor pays it one unit short) | NP01 | NP01 | CONFIRMED |
| Q2 | KobPair | a bid receives exactly n | NP05 | NP05 | CONFIRMED |
| Q3 | KobPair | a bid pays exactly its floor | NP03, NP04 | NP03 NP04 | CONFIRMED |
| Q3a | KobPair | a bid pays no less than its floor (it pays exactly its quote: takeable evidence) | NP04 | NP04 | CONFIRMED |
| Q3b | KobPair | a bid pays no more than its floor | NP03 | NP03 | CONFIRMED |
| Q4 | KobPair | C1: an ask's custody is its whole amountLeft (every displayed amount takeable; never evidence otherwise) | NP12a, NP12b, NC40, NC43, NC40u, NC43u | NP12a NP12b NC40 NC43 NC40u NC43u | CONFIRMED |
| Q5 | KobPair | C1: the escrow pays the quote (an unfunded bid can be neither filled nor evidence) | NP10, NC41, NC41u | NP10 NC41 NC41u | CONFIRMED |
| Q6 | KobPair | C1: the order UTXO funds its delivery carrier and tip (no filler-funded carrier wall) | NP11a, NP11b, NC42a, NC42b, NC42au, NC42bu | NP11a NP11b NC42a NC42b NC42au NC42bu | CONFIRMED |
| Q7 | KobPair | a rest keeps something in the custody (no continuation with an empty escrow) | NP13 | NP13 | CONFIRMED |
| Q8 | KobPair | the KAS tip is the floor | NP118a, NP118b | NP118a NP118b | CONFIRMED |
| T1 | KobPair | the maker's T output pinned (owner, owner type, extension, borrowing, minter; one delivery for two orders; the token programs also refuse NP92) | NP90tk, NP90tr, NP91tk, NP91tr, NP92tk, NP92tr, NP93tk, NP40a, NP40b | NP90tk NP91tk NP92tk(F) NP93tk NP90tr NP91tr NP92tr(F) NP40a NP40b | CONFIRMED |
| T2k | KobPair | KCC-20 delivery owned by a key (scheme 0x00) | NP91tk | NP91tk | CONFIRMED |
| T2r | KobPair | KRON delivery of id_type 3 (address) | NP91tr | NP91tr | CONFIRMED |
| T3 | KobPair | the delivery carries the T covenant | NP94a, NP94b | NP94a NP94b | CONFIRMED |
| T4 | KobPair | the T template source's prefix / suffix hash (a planted look-alike; the T program also refuses the bound look-alike output) | NP95a, NP95b; stay rejected: NP96a, NP96b | NP95a(F) NP95b(F) | CONFIRMED |
| T5 | KobPair | the T template source carries the T covenant: defence in depth (any T-covenant input is a genuine T program input whose bytes the hash pins; another program fails the hash) | stay rejected: NP96a, NP96b |  | held (defence in depth) |
| T6k | KobPair | tFamily is 1 or 2 (hostile field read as KCC-20) | NP23tk | NP23tk | CONFIRMED |
| O1 | KobPair | the one S output (rest owned by the order / return / refund to the maker, exact amount) at the custody index / output i (a refund custody at another index is held by the covenant-id pin too) | NP97a, NP97b, NP98a, NP98b, NP99a, NP99b, NP76a, NP76b, NP41a, NP41b; stay rejected: NP78a, NP78b | NP97a NP98a NP99a NP97b NP98b NP99b NP76a NP76b NP41a NP41b | CONFIRMED |
| O2 | KobPair | the S output carries the S covenant (a refund custody at another index is held by the script pin too) | NP100a, NP100b; stay rejected: NP78a, NP78b | NP100a NP100b | CONFIRMED |
| O12 | KobPair | both S-output pins removed: a refund custody moved to another index leaves output i a plain output of the order value | NP78a, NP78b | NP76a NP78a NP76b NP78b | CONFIRMED |
| K1 | KobPair | one continuation (a second output bound to the id is a forged UTXO of it; none fails OpCovOutputIdx anyway) | NP42a, NP42b; stay rejected: NP43a, NP43b | NP42a NP42b | CONFIRMED |
| K2 | KobPair | the continuation is this script with amountLeft - n and custody - sOut | NP110a, NP110b, NP111a, NP111b | NP110a NP111a NP110b NP111b | CONFIRMED |
| K3 | KobPair | the continuation keeps the order value minus the delivery carrier and the tip | NP112a, NP112b | NP112a NP118a NP112b NP118b | CONFIRMED |
| K4 | KobPair | the custody rest keeps its carrier | NP113a, NP113b | NP113a NP113b | CONFIRMED |
| K5 | KobPair | the delivery carries deliveryCarrier | NP114a, NP114b | NP114a NP114b | CONFIRMED |
| K6 | KobPair | FOK: all or nothing | NP53 | NP53 | CONFIRMED |
| K7 | KobPair | a terminating fill leaves no output bound to the order id | NP44a, NP44b | NP44a NP44b | CONFIRMED |
| K8 | KobPair | the IOC return keeps its carrier | NP115a, NP115b | NP115a NP115b | CONFIRMED |
| K9 | KobPair | IOC / done: the maker gets the order value back but the tip | NP116a, NP116b | NP116a NP116b | CONFIRMED |
| K10 | KobPair | sold out: every carrier back to the maker but the tip | NP117a, NP117b | NP117a NP117b | CONFIRMED |
| X1 | KobPair | cancel: SIGHASH_ALL only | NP79a, NP79b | NP79a NP79b | CONFIRMED |
| X2 | KobPair | cancel: the maker signs | NP80a, NP80b | NP80a NP80b | CONFIRMED |

Collateral flips (scenarios accepted besides the expected ones):

- G2: NP34t0 NP34t1 NP34t2 NP34t3 NP34t4 NP34t5 NP34t6 NP34t7 NP35t
- GS1: NP30a0 NP31a0 NP30a1 NP31a1 NP30a2 NP31a2 NP32a NP30b0 NP31b0 NP30b1 NP31b1 NP30b2 NP31b2 NP32b
- E1c: NP73a NP73b
- O12: NP76a NP76b
- K3: NP118a NP118b

## cond-pair

pair orders: KobCondPair (excl. repeat merges); test binary `kob_cond_pair_tests`; contracts `contracts/v2`. 116/116 confirmed, 10/10 defence-in-depth held (expected NOT to flip).
Baseline (no mutation): cond_pair_updates clean, cond_pair_lifecycle clean, cond_pair_settlement clean, cond_pair_evidence_ev1 clean, cond_pair_evidence_ev0 clean, cond_pair_evidence_tk clean, cond_pair_evidence_exposure clean, cond_pair_trailing clean, cond_pair_fills clean, cond_pair_overflow clean, cond_pair_custody clean, cond_pair_evidence_misc clean, cond_pair_carriers clean.

| id | contract | check ablated | expected | flipped | status |
|---|---|---|---|---|---|
| S1 | KobCondPair | the order is the only input of its covenant id (a sibling UTXO of the id) | NC17a, NC17b | NC17a NC17b | CONFIRMED |
| C1 | KobCondPair | exact custody: held == custody | NC04a, NC04b | NC04a NC04b | CONFIRMED |
| C2 | KobCondPair | a refund needs a positive custody (a zero custody leaves output i unpinned) | NC82a, NC82b | NC82a NC82b(F) NC82a(F) NC82b | CONFIRMED |
| G1 | KobCondPair | no S input owned by this id but the custody (fill, refund; the update spends none) | NC06a, NC06b, NC08a, NC08b, NC76 | NC06a NC08a NC06b NC08b NC76 | CONFIRMED |
| G2 | KobCondPair | no T input owned by this id | NC07a, NC07b, NC09a, NC09b, NC77 | NC07a NC09a NC07b NC09b NC18t0 NC18t1 NC18t2 NC18t3 NC18t4 NC18t5 NC18t6 NC18t7 NC19t(F) NC77 | CONFIRMED |
| G3 | KobCondPair | an update spends no S input of the order (cust = -1 so the custody is not exempted) | NC76 | NC76 | CONFIRMED |
| GS0 | KobCondPair | noStrays scan slot 0: a stray of the order at the input of slot 0 of its token | NC18t0 | NC09a NC09b NC18t0 | CONFIRMED |
| GS1 | KobCondPair | noStrays scan slot 1: a stray of the order at the input of slot 1 of its token | NC18t1 | NC06a NC07a NC08a NC06b NC07b NC08b NC18t1 | CONFIRMED |
| GS2 | KobCondPair | noStrays scan slot 2: a stray of the order at the input of slot 2 of its token | NC18t2 | NC18t2 | CONFIRMED |
| GS3 | KobCondPair | noStrays scan slot 3: a stray of the order at the input of slot 3 of its token | NC18t3 | NC18t3 | CONFIRMED |
| GS4 | KobCondPair | noStrays scan slot 4: a stray of the order at the input of slot 4 of its token | NC18t4 | NC18t4 | CONFIRMED |
| GS5 | KobCondPair | noStrays scan slot 5: a stray of the order at the input of slot 5 of its token | NC18t5 | NC18t5 | CONFIRMED |
| GS6 | KobCondPair | noStrays scan slot 6: a stray of the order at the input of slot 6 of its token | NC18t6 | NC18t6 | CONFIRMED |
| GS7 | KobCondPair | noStrays scan slot 7: a stray of the order at the input of slot 7 of its token | NC18t7 | NC18t7 | CONFIRMED |
| GB | KobCondPair | noStrays bound: at most MAX_TOK_IN = 8 inputs of the token (a 9th is not scanned; the KCC-20 program refuses it on its own input) | NC19t | NC19t(F) | CONFIRMED |
| N1 | KobCondPair | n <= amountLeft | NC05 | NC05 | CONFIRMED |
| N2 | KobCondPair | an ask receives at least the ceil | NC01 | NC01 | CONFIRMED |
| N2r | KobCondPair | an ask's leg price is rounded up | NC01 | NC01 | CONFIRMED |
| N3 | KobCondPair | a bid pays at most the floor | NC02 | NC02 | CONFIRMED |
| N4 | KobCondPair | a bid receives exactly n | NC03 | NC03 | CONFIRMED |
| O1 | KobCondPair | the maker's token / the S output pinned (owner, amount, index) | NC11a, NC11b, NC84a, NC84b | NC11a NC11b NC84a NC84b | CONFIRMED |
| O2 | KobCondPair | the S output carries the S covenant | NC16a, NC16b | NC16a NC16b | CONFIRMED |
| O3 | KobCondPair | the maker's token output at i is pinned (delivery / profit) | NC13a, NC13b | NC13a NC13b | CONFIRMED |
| O4 | KobCondPair | the maker's token output carries its token covenant | NC14a, NC14b | NC14a NC14b | CONFIRMED |
| K1 | KobCondPair | one continuation on a partial fill | NC10a, NC10b | NC10a NC10b | CONFIRMED |
| EVPUSH | KobCondPair | rd: the evidence sigscript starts with the 8-byte fill push (a cancel ground to read as a fill) | NC2Ca, NC2Cb, NC3Ca, NC3Cb | NC2Ca NC2Cb NC3Ca NC3Cb | CONFIRMED |
| EVN | KobCondPair | rd: the evidence is filled (n > 0) | NC20a, NC20b, NC30a, NC30b | NC20a(F) NC20b(F) NC30a(F) NC30b(F) | CONFIRMED |
| EVMT | KobCondPair | rd: n >= minTouch | NC24a, NC24b, NC34a, NC34b | NC24a(F) NC24b(F) NC34a NC34b | CONFIRMED |
| EVID | KobCondPair | rd: token and scale (mode 1: side, S, T and both scales) are the order's | NC22a, NC22b, NC23a, NC23b, NC32a, NC32b, NC33a, NC33b, NC35a, NC35b | NC22a(F) NC23a(F) NC22b(F) NC23b(F) NC32a(F) NC33a(F) NC35a(F) NC32b(F) NC33b(F) NC35b(F) | CONFIRMED |
| EVSLOPE | KobCondPair | rd: the evidence is not decaying | NC21a, NC21b, NC31a, NC31b | NC21a(F) NC21b(F) NC31a(F) NC31b(F) | CONFIRMED |
| EVREST | KobCondPair | rd: rested >= minRestDaa before the lock (every exposure term) | NC50, NC52, NC53, NC51a, NC3Ea, NC3Eb | NC50 NC52(F) NC53(F) NC51a NC3Ea NC3Eb | CONFIRMED |
| EVINT | KobCondPair | rd: the interval exposure term | NC53 | NC53(F) | CONFIRMED |
| EVACT | KobCondPair | rd: the activeFrom exposure term | NC52 | NC52(F) | CONFIRMED |
| EVTKDAA | KobCondPair | rd: the ask evidence's custody DAA exposure term | NC51a | NC51a | CONFIRMED |
| EVTK | KobCondPair | rd: the ask evidence's custody is a token input of its token | NC59a | NC59a | CONFIRMED |
| EVTKOWN | KobCondPair | rd: the ask evidence's custody is owned by the evidence ask (covenant marker) | NC57a | NC57a | CONFIRMED |
| EVTKTPL | KobCondPair | rd: the ask / pair evidence's custody is a holder of the token's program (template hash; P2SH kept): an old look-alike holder of another program refused | NC5Ha, NC3Ha, NC3Hb | NC5Ha(F) NC3Ha(F) NC3Hb(F) | CONFIRMED |
| EVTKP2SH | KobCondPair | rd: the ask / pair evidence's custody is the P2SH of the holder redeem it shows (template hash kept): an old non-P2SH input with the genuine redeem refused | NC5Sa, NC3Sa, NC3Sb | NC5Sa(F) NC3Sa(F) NC3Sb(F) | CONFIRMED |
| EVBPUSH | KobCondPair | evLeg: the B evidence sigscript starts with the 8-byte fill push (a cancel ground to read as a fill) | NC5Da, NC5Db | NC5Da NC5Db | CONFIRMED |
| EVBN | KobCondPair | evLeg: the B evidence is filled (n > 0) | NC2Na, NC2Nb | NC2Na(F) NC2Nb(F) | CONFIRMED |
| EVBMT | KobCondPair | evLeg: n_B >= ceil(minTouch * stop / scale(A)) | NC25a, NC25b | NC25a(F) NC25b(F) | CONFIRMED |
| EVBTOK | KobCondPair | evLeg: the B evidence is of token B | NC2Ta, NC2Tb | NC2Ta(F) NC2Tb(F) | CONFIRMED |
| EVBSCALE | KobCondPair | evLeg: the B evidence quotes at B's scale | NC28a, NC28b | NC28a(F) NC28b(F) | CONFIRMED |
| EVBSLOPE | KobCondPair | evLeg: the B evidence is not decaying | NC29a, NC29b | NC29a(F) NC29b(F) | CONFIRMED |
| EVBREST | KobCondPair | evLeg: rested >= minRestDaa (every exposure term) | NC2Ea, NC2Eb, NC2Ia, NC2Ib, NC2Aa, NC2Ab, NC51b | NC2Ea NC2Ia(F) NC2Aa(F) NC2Eb NC2Ib(F) NC2Ab(F) NC51b | CONFIRMED |
| EVBINT | KobCondPair | evLeg: the interval exposure term | NC2Ia, NC2Ib | NC2Ia(F) NC2Ib(F) | CONFIRMED |
| EVBACT | KobCondPair | evLeg: the activeFrom exposure term | NC2Aa, NC2Ab | NC2Aa(F) NC2Ab(F) | CONFIRMED |
| EVBTKDAA | KobCondPair | evLeg: the ask evidence's custody DAA exposure term | NC51b | NC51b | CONFIRMED |
| EVBTK | KobCondPair | evLeg: the ask evidence's custody is a token input of its token | NC59b | NC59b | CONFIRMED |
| EVBTKOWN | KobCondPair | evLeg: the ask evidence's custody is owned by the evidence ask | NC57b | NC57b | CONFIRMED |
| EVBTKTPL | KobCondPair | evLeg: the B ask evidence's custody is a holder of token B's program (template hash; P2SH kept): an old look-alike holder of another program refused | NC5Hb | NC5Hb(F) | CONFIRMED |
| EVBTKP2SH | KobCondPair | evLeg: the B ask evidence's custody is the P2SH of the holder redeem it shows (template hash kept): an old non-P2SH input with the genuine redeem refused | NC5Sb | NC5Sb(F) | CONFIRMED |
| EVHASH | KobCondPair | the evidence template hash (a look-alike P2SH redeem; the wrong side and a wrong input are also refused by the P2SH check or the fill push) | NC58; stay rejected: NC54, NC55, NC56, NC26a, NC26b | NC5Ha(F) NC3Ha(F) NC5Hb(F) NC3Hb(F) NC58 | CONFIRMED |
| EVP2SH | KobCondPair | the evidence is a P2SH spend of the redeem it shows (a planted UTXO with genuine redeem bytes) | NC5P; stay rejected: NC54, NC55, NC56, NC26a, NC26b | NC5Sa(F) NC3Sa(F) NC5Sb(F) NC3Sb(F) NC5P | CONFIRMED |
| EVARM | KobCondPair | the implied rate reaches the stop / the trail is valid (a >= q hi, a <= q lo) | NC36a, NC36b, NC60a, NC60b | NC36a(F) NC36b(F) NC60a NC60b | CONFIRMED |
| TRMAX | KobCondPair | the trail is maximal (a < q2 hi, a > q2 lo) | NC61a, NC61b | NC61a NC61b | CONFIRMED |
| EVARMH | KobCondPair | hi branch: a buy stop arms only at a rate >= its stop; a sell stop trails only to a valid stop | NC36b, NC60a | NC36b(F) NC60a | CONFIRMED |
| EVARML | KobCondPair | lo branch: a sell stop arms only at a rate <= its stop; a buy stop trails only to a valid stop | NC36a, NC60b | NC36a(F) NC60b | CONFIRMED |
| TRMAXH | KobCondPair | hi branch: a sell stop trails maximally | NC61a | NC61a | CONFIRMED |
| TRMAXL | KobCondPair | lo branch: a buy stop trails maximally | NC61b | NC61b | CONFIRMED |
| TRK | KobCondPair | k > 0 in the trail branch (k <= 0 is an arm; the validity / maximality checks also bind) | stay rejected: NC60a, NC60b |  | held (defence in depth) |
| TRWAIT | KobCondPair | at most one ratchet per trailWait | NC63a, NC63b | NC63a NC63b | CONFIRMED |
| TRCAPA | KobCondPair | ASK trail stays below tpPrice | NC64a | NC64a | CONFIRMED |
| TRCAPB | KobCondPair | BID trail stays above the floor (max(tpPrice, 0)) | NC64b | NC64b | CONFIRMED |
| U1 | KobCondPair | an update has n == 0 | NC70 | NC70 | CONFIRMED |
| U2 | KobCondPair | upd is 0 or 1 | NC71 | NC71 | CONFIRMED |
| U3 | KobCondPair | only an unarmed stop is updated | NC72 | NC72 | CONFIRMED |
| U4 | KobCondPair | the order has a stop leg | NC73 | NC73 | CONFIRMED |
| U5 | KobCondPair | an update waits for activeFrom | NC78 | NC78 | CONFIRMED |
| U6 | KobCondPair | the keeper takes at most keeperTip (the continuation keeps value - keeperTip) | NC74 | NC74 | CONFIRMED |
| U7 | KobCondPair | keeperTip >= 0 (hostile field) | NC75 | NC75 | CONFIRMED |
| L1 | KobCondPair | a positive leg price (a sell stop with slipBps 10000 trades at 0; a KRON T also refuses the empty delivery; a zero stopPrice is held by `stopPrice > 0` too) | NC15; stay rejected: NC127 | NC15(F) NC15 | CONFIRMED |
| E1 | KobCondPair | refund only once due (expiry, 90 days idle) | NC80a, NC80b, NC81a, NC81b | NC80a NC81a NC80b NC81b | CONFIRMED |
| E2 | KobCondPair | a refund pays the maker everything but refundTip | NC83a, NC83b | NC83a NC83b | CONFIRMED |
| E3 | KobCondPair | a refund (or a terminating fill) leaves no output bound to the order id | NC85a, NC85b | NC85a NC85b | CONFIRMED |
| X1 | KobCondPair | cancel: SIGHASH_ALL only | NC86a, NC86b | NC86a NC86b | CONFIRMED |
| X2 | KobCondPair | cancel: the maker signs | NC87a, NC87b | NC87a NC87b | CONFIRMED |
| C3r | KobCondPair | KRON custody owned by this order (the KRON program also refuses an owner not spent) | NC100sr | NC100sr(F) | CONFIRMED |
| C3k | KobCondPair | KCC-20 custody owned by this order (the KCC-20 program also refuses an owner not spent) | NC100sk | NC100sk(F) | CONFIRMED |
| C4r | KobCondPair | KRON custody of owner type 2 (covenant id) | NC101sr | NC101sr(F) | CONFIRMED |
| C5r | KobCondPair | KRON custody not a minter | NC102sr | NC102sr | CONFIRMED |
| C4k | KobCondPair | KCC-20 custody of owner scheme 0x04, borrowing disabled | NC101sk | NC101sk | CONFIRMED |
| C6k | KobCondPair | sFamily is 1 or 2 (hostile field read as KCC-20) | NC102sk | NC102sk | CONFIRMED |
| C6tk | KobCondPair | tFamily is 1 or 2 (hostile field read as KCC-20) | NC103tk | NC103tk | CONFIRMED |
| C0 | KobCondPair | the custody carries the S covenant (not a UTXO of another token owned by the order) | NC104a, NC104b | NC104a NC104b | CONFIRMED |
| CP2SH | KobCondPair | the custody is a P2SH spend of its redeem (a planted UTXO of the S covenant id) | NC105a, NC105b | NC105a NC105b | CONFIRMED |
| CHASH | KobCondPair | the custody's template is S's (a planted look-alike P2SH) | NC106a, NC106b | NC106a NC106b | CONFIRMED |
| F1 | KobCondPair | no fill before activeFrom | NC110 | NC110 | CONFIRMED |
| F2 | KobCondPair | fill n > 0: defence in depth (n = -1 under a hostile minFill: an ask then needs sOut == -1, a bid tOut == -1, negative token amounts) | stay rejected: NC128 |  | held (defence in depth) |
| F3 | KobCondPair | minimum fill | NC111 | NC111 | CONFIRMED |
| F4 | KobCondPair | tip >= 0 (hostile field) | NC112 | NC112 | CONFIRMED |
| F5 | KobCondPair | deliveryCarrier >= 0 (hostile field) | NC113 | NC113 | CONFIRMED |
| F6 | KobCondPair | scale(A) > 0 (a negative scale makes the ceil of the quote negative) | NC114 | NC114 | CONFIRMED |
| F7 | KobCondPair | a TP fill needs tpPrice > 0 (defence in depth: a zero TP is also a zero legPrice) | stay rejected: NC115 |  | held (defence in depth) |
| F7L | KobCondPair | tpPrice > 0 and legPrice > 0 both removed: a TP fill of an order without a TP leg | NC115 | NC115 | CONFIRMED |
| F8 | KobCondPair | leg is 0 or 1 (leg 2 would fill an unarmed stop without evidence) | NC116 | NC116 | CONFIRMED |
| F9 | KobCondPair | the stop leg needs stopPrice > 0: defence in depth (a zero stop is a zero legPrice) | stay rejected: NC127 |  | held (defence in depth) |
| F9L | KobCondPair | stopPrice > 0 and legPrice > 0 both removed: an armed stop of hostile stopPrice 0 sells for one unit of B | NC127 | NC127 | CONFIRMED |
| F10 | KobCondPair | slipBps >= 0 (hostile field) | NC118 | NC118 | CONFIRMED |
| F11 | KobCondPair | slipBps <= 10000 | NC119 | NC119 | CONFIRMED |
| F12 | KobCondPair | the auction time t is proven by CLTV | NC120 | NC120 | CONFIRMED |
| F13 | KobCondPair | the auction time t is not before the origin | NC121 | NC121 | CONFIRMED |
| F14 | KobCondPair | the auction band opens linearly over bandDaa | NC122 | NC122 | CONFIRMED |
| F15 | KobCondPair | the fill that arms a stop with bandDaa > 0 trades at the stop itself | NC123 | NC123 | CONFIRMED |
| F16 | KobCondPair | stopPrice <= MAX_STOP (defence in depth: a larger stop overflows the quote product anyway) | stay rejected: NC92 |  | held (defence in depth) |
| F17 | KobCondPair | an ask releases exactly n | NC124 | NC124 | CONFIRMED |
| F18 | KobCondPair | a bid pays a non-negative amount | NC125 | NC125 | CONFIRMED |
| F19 | KobCondPair | the escrow pays the quote (an unfunded bid cannot be filled) | NC126 | NC126 | CONFIRMED |
| F20 | KobCondPair | side is 1 or 2 (a fill of hostile side 3) | NC117 | NC117 | CONFIRMED |
| F21 | KobCondPair | side is 1 or 2 in the arming block: defence in depth (pairEv reads a side-3 order as an ASK, so its evidence token check refuses the BID evidence) | stay rejected: NC142 |  | held (defence in depth) |
| T1 | KobCondPair | the T template source's hash (a planted look-alike; the T program also refuses the bound look-alike output) | NC130a, NC130b; stay rejected: NC131a, NC131b | NC130a(F) NC130b(F) | CONFIRMED |
| T2 | KobCondPair | the T template source carries the T covenant: defence in depth (any T-covenant input is a genuine T program input whose bytes the hash pins; another program fails the hash) | stay rejected: NC131a, NC131b |  | held (defence in depth) |
| K2 | KobCondPair | the continuation is this script with the new mutable window | NC132a, NC132b | NC132a NC132b | CONFIRMED |
| K3 | KobCondPair | the continuation keeps its floor (order value - deliveryCarrier - tip) | NC133a, NC133b | NC133a NC133b | CONFIRMED |
| K4 | KobCondPair | the custody rest keeps its carrier | NC134a, NC134b | NC134a NC134b | CONFIRMED |
| K5 | KobCondPair | the delivery carries deliveryCarrier | NC135a, NC135b | NC135a NC135b | CONFIRMED |
| K6 | KobCondPair | a return keeps its carrier | NC136a | NC136a | CONFIRMED |
| K7 | KobCondPair | done with a return: the maker gets the order value but the tip | NC136b | NC136b | CONFIRMED |
| K8 | KobCondPair | sold out: every carrier back to the maker but the tip | NC137a, NC137b | NC137a NC137b | CONFIRMED |
| Q7 | KobCondPair | a rest keeps something in the custody (no continuation with an empty escrow) | NC138 | NC138 | CONFIRMED |
| EVMODE | KobCondPair | evMode is 0 or 1 | NC140 | NC140 | CONFIRMED |
| EVB0 | KobCondPair | the B quote is positive (a buy stop would arm on any B ask at 0) | NC141 | NC141(F) | CONFIRMED |
| TRGAP | KobCondPair | trailGap >= 0 (hostile field: a sell stop would trail above the rate) | NC143 | NC143 | CONFIRMED |
| TRSTEP | KobCondPair | trailStep > 0: defence in depth (a zero / negative step makes validity and maximality contradict) | stay rejected: NC144 |  | held (defence in depth) |
| TRX | KobCondPair | a buy stop's s' - gap >= 0: defence in depth (a negative x makes the validity a <= floor(x * b / sB) fail) | stay rejected: NC145 |  | held (defence in depth) |
| EVTPL | KobCondPair | evidence template hash and P2SH both removed: the wrong side (the order own counterparties) and the order / a token / a P2PK input are still refused (state read at the other template offsets fails the token / scale identity; the fill push refuses token / P2PK inputs) | stay rejected: NC26a, NC26b, NC54, NC55, NC56 | NC5Ha(F) NC5Sa(F) NC3Ha(F) NC3Sa(F) NC5Hb(F) NC5Sb(F) NC3Hb(F) NC3Sb(F) NC58 NC5P | held (defence in depth) |

Collateral flips (scenarios accepted besides the expected ones):

- G2: NC18t0 NC18t1 NC18t2 NC18t3 NC18t4 NC18t5 NC18t6 NC18t7 NC19t
- GS0: NC09a NC09b
- GS1: NC06a NC07a NC08a NC06b NC07b NC08b
- EVHASH: NC5Ha NC3Ha NC5Hb NC3Hb
- EVP2SH: NC5Sa NC3Sa NC5Sb NC3Sb
- EVTPL: NC5Ha NC5Sa NC3Ha NC3Sa NC5Hb NC5Sb NC3Hb NC3Sb NC58 NC5P

## ifd-pair

KobIfdPair pair order; test binary `kob_ifd_pair_tests`; contracts `contracts/v2`. 122/122 confirmed, 4/4 defence-in-depth held (expected NOT to flip).
Baseline (no mutation): ifd_fill_sell clean, ifd_fill_buy clean, ifd_update clean, ifd_strays clean, ifd_custody clean, ifd_encoding clean, ifd_aliasing clean, ifd_exit_genesis clean, ifd_merge_rules clean, ifd_refund clean, ifd_evidence clean, ifd_merge clean, ifd_hostile clean, ifd_update_rules clean, ifd_refund_rules clean, ifd_ev_template clean, ifd_outputs clean, ifd_stray_slots clean, ifd_custody_marks clean, ifd_fill_rules clean, ifd_ev_tk clean, ifd_ev_battery clean, ifd_merge_refund clean, ifd_limits clean.

| id | contract | check ablated | expected | flipped | status |
|---|---|---|---|---|---|
| IF1 | KobIfdPair | buy-first: the B released is at most floor(n * p / scale(A)) | NI01 | NI01 | CONFIRMED |
| IF2 | KobIfdPair | sell-first: the proceeds put into the exit custody are at least ceil(n * p / scale(A)) | NI05 | NI05 | CONFIRMED |
| IF3 | KobIfdPair | the exit custody (output i) carries deliveryCarrier | NI03, NI06 | NI03 NI06 | CONFIRMED |
| IF4 | KobIfdPair | the continuation holds at least its floor (also the update keeper floor, NI30) | NI04, NI07, NIM5 | NI04 NI07 NIM5 | CONFIRMED |
| IF12 | KobIfdPair | an arming keeper takes at most keeperTip from the entry UTXO | NI30 | NI30 | CONFIRMED |
| IF5 | KobIfdPair | each custody (A, B escrow, B prefund) holds exactly its state amount (heldGx) | NI09, NI10, NI11 | NI09 NI10 NI11 | CONFIRMED |
| IF6 | KobIfdPair | no input of A owned by this id other than the A custody (fill, update) | NI12, NI15 | NI12 NI15 | CONFIRMED |
| IF7 | KobIfdPair | no input of B owned by this id other than the B custody (fill, refund, update spends none) | NI13, NI14, NI25 | NI13 NI14 NI25 | CONFIRMED |
| IF8 | KobIfdPair | the exit's custody of n (buy) / proceeds+prefund (sell) is pinned at output i (pinTok) | NI17, NI23 | NI17 NI23 | CONFIRMED |
| IF9 | KobIfdPair | the exit output equals the committed exit state (amountLeft := n, custody, repeat fields): DEFENCE IN DEPTH with the genesis id IF10 and the consensus genesis binding. Any deviation of the exit spk changes its consensus covenant id, so the output cannot both carry a wrong spk and the id blake2b(exitSpk) requires (IF9b removes both and flips NI18). | stay rejected: NI18 |  | held (defence in depth) |
| IF9b | KobIfdPair | the exit spk pin and the genesis id together bind amountLeft := n (removing both lets the exit commit amountLeft = n + 1) | NI18 | NI18 NI19 | CONFIRMED |
| IF10 | KobIfdPair | the exit is the fresh single-output genesis this input derives (keyed blake2b over the outpoint and the one output): a two-output genesis group has a different consensus id. Holds NI18 (the spk pin IF9 also catches an edited amountLeft). | NI19; stay rejected: NI18 | NI19 | CONFIRMED |
| IF11 | KobIfdPair | the exit prefix hashes to COND_TPL: DEFENCE IN DEPTH. A wrong prefix also changes the built exitSpk (caught by IF9) and the genesis id (IF10), so it is triple-guarded; removing this check alone leaves NI20 rejected. | stay rejected: NI20 |  | held (defence in depth) |
| IF13 | KobIfdPair | update: upd is 0 or 1 (upd == 1 in the update branch) | NI26 | NI26 | CONFIRMED |
| IF14 | KobIfdPair | the entry is the only input of its covenant id (the attack plants a second input of the id; that sibling input also rejects, so only the order input flips) | NI28 | NI28(F) | CONFIRMED |
| IF17 | KobIfdPair | a merge into an empty buy-first entry funds the NEW B escrow with exitCarrier | NIM1 | NIM1 | CONFIRMED |
| IF18 | KobIfdPair | a merge into an empty sell-first entry funds the NEW A custody with exitCarrier | NIM2 | NIM2 | CONFIRMED |
| IF19 | KobIfdPair | the merged buy-first escrow grows by exactly budget(m) (the B output is pinned to custody + budget) | NIM3 | NIM3 | CONFIRMED |
| IF20 | KobIfdPair | the merged sell-first A custody grows by exactly m (the A output is pinned to amountLeft + m) | NIM4 | NIM4 | CONFIRMED |
| IF21 | KobIfdPair | rd(): the first evidence read (a pair order in mode 1, the A leg in mode 0) was exposed >= minRestDaa before the fill | NEV1, NEV3 | NEV1 NEV3 | CONFIRMED |
| IF22 | KobIfdPair | evLeg(): the mode-0 B leg (a KAS-book order of B) was exposed >= minRestDaa before the fill | NEV2 | NEV2 | CONFIRMED |
| IF15 | KobIfdPair | refund only from the expiry / 90-day idle time | NI33 | NI33 | CONFIRMED |
| IF16 | KobIfdPair | refund pays the maker everything but refundTip | NI34 | NI34 | CONFIRMED |
| IF23 | KobIfdPair | a fill waits for activeFrom (CLTV) | NIF01 | NIF01 | CONFIRMED |
| IF24 | KobIfdPair | n <= amountLeft (a fill of amountLeft + 1 would write an exit of more than the entry holds / buys) | NIF02 | NIF02 | CONFIRMED |
| IF25 | KobIfdPair | n >= minFill unless the fill takes everything left (one base unit below minFill refused) | NIF03 | NIF03 | CONFIRMED |
| IF26 | KobIfdPair | tip >= 0 (hostile field: a negative tip raises the continuation floor; the filler funds it) | NIF04 | NIF04 | CONFIRMED |
| IF27 | KobIfdPair | deliveryCarrier >= 0 (hostile field) | NIF05 | NIF05 | CONFIRMED |
| IF28 | KobIfdPair | exitCarrier >= 0 (hostile field) | NIF06 | NIF06 | CONFIRMED |
| IF29 | KobIfdPair | fill: a buy-stop entry has entryStop <= price | NIF07 | NIF07 | CONFIRMED |
| IF30 | KobIfdPair | fill: a sell-stop entry has entryStop >= price | NIF08 | NIF08 | CONFIRMED |
| IF31 | KobIfdPair | the auction time t is proven by CLTV (tx.daa >= t) | NIF09 | NIF09 | CONFIRMED |
| IF32 | KobIfdPair | the auction time t is not before the auction origin | NIF10 | NIF10 | CONFIRMED |
| IF33 | KobIfdPair | the entry quote p is positive (a price-0 entry never trades) | NIF11 | NIF11 | CONFIRMED |
| IF34 | KobIfdPair | a booking dates its cycle (rptUntil) by the entry UTXO's DAA, never by the filler's t argument (dated by t, a filler pushes rptUntil out) | NIF13 | NIF13 | CONFIRMED |
| IF35 | KobIfdPair | buy-first: the B released is not negative (sign check) | NIF14 | NIF14 | CONFIRMED |
| IF36 | KobIfdPair | buy-first: the release does not exceed the escrow (bNew >= 0; sign check) | NIF15 | NIF15 | CONFIRMED |
| IF37 | KobIfdPair | sell-first: prefund >= 0 (hostile field: a negative pre(n) moves the exit custody into the prefund) | NIF16 | NIF16 | CONFIRMED |
| IF38 | KobIfdPair | sell-first: a continuing fill takes pre(n) from a prefund custody that holds it | NIF17 | NIF17 | CONFIRMED |
| IF39 | KobIfdPair | a continuing fill: the exit UTXO carries exitCarrier | NIO4 | NIO4 | CONFIRMED |
| IF40 | KobIfdPair | a continuing fill: the A custody rest keeps its carrier | NIO5 | NIO5 | CONFIRMED |
| IF41 | KobIfdPair | a continuing fill: the B custody rest keeps its carrier | NIO6 | NIO6 | CONFIRMED |
| IF42 | KobIfdPair | a terminating buy-first fill: the escrow return to the maker keeps its carrier | NIO7 | NIO7 | CONFIRMED |
| IF43 | KobIfdPair | a terminating fill: the last exit takes all the KAS left | NIO8 | NIO8 | CONFIRMED |
| IF44 | KobIfdPair | a continuing entry has exactly one output bound to its id | NIO1 | NIO1 | CONFIRMED |
| IF45 | KobIfdPair | the continuation is this script with the new mutable window | NIO2 | NIO2 | CONFIRMED |
| IF46 | KobIfdPair | a terminated entry leaves no output bound to its id | NIO3 | NIO3 | CONFIRMED |
| IF47 | KobIfdPair | pinTok: the pinned token output is bound to the token covenant (an unbound look-alike of the exit custody refused). Holds NI23 / NIO10 (a custody moved off its index: the spk pin IF8b also refuses the plain output there; IF8c removes both) | NIO9; stay rejected: NIO10, NI23 | NIO9 | CONFIRMED |
| IF8b | KobIfdPair | pinTok: the pinned token output has exactly the token state on the token template (amount, owner). Holds NI23 / NIO10 (a custody moved off its index: the covenant-id pin IF47 also refuses the plain output there) | NI17, NIM3, NIM4; stay rejected: NI23, NIO10 | NI17 NIM3 NIM4 | CONFIRMED |
| IF8c | KobIfdPair | pinTok spk pin AND covenant-id pin removed together: a custody (the exit custody, the escrow rest) may then sit at any index (IF8b / IF47 each alone hold) | NI23, NIO10 | NI23 NIO9 NIO10 | CONFIRMED |
| IF48 | KobIfdPair | side is 1 or 2 (hostile side 3 read as sell-first) | NIH5 | NIH5 | CONFIRMED |
| IF49 | KobIfdPair | scale(A) > 0 (a hostile negative scale makes every quote negative; zero divides by zero anyway) | NIH6 | NIH6 | CONFIRMED |
| IF50k | KobIfdPair | aFamily is 1 or 2 (hostile family 3 read as KCC-20) | NIH7k | NIH7k | CONFIRMED |
| IF51k | KobIfdPair | bFamily is 1 or 2 (hostile family 3 read as KCC-20) | NIH8k | NIH8k | CONFIRMED |
| IF52 | KobIfdPair | cancel: SIGHASH_ALL only | NIC1 | NIC1 | CONFIRMED |
| IF53 | KobIfdPair | cancel: the maker signs | NIC2 | NIC2 | CONFIRMED |
| IF54 | KobIfdPair | update: only a stop entry (entryStop > 0) | NIU1 | NIU1 | CONFIRMED |
| IF55 | KobIfdPair | update: a buy-stop entry has entryStop <= price | NIU2 | NIU2 | CONFIRMED |
| IF56 | KobIfdPair | update: a sell-stop entry has entryStop >= price | NIU3 | NIU3 | CONFIRMED |
| IF57 | KobIfdPair | update: the entry has something left to trade | NIU4 | NIU4 | CONFIRMED |
| IF58 | KobIfdPair | update: only an unarmed entry (an armed one would restart its auction at armed = 1) | NIU5 | NIU5 | CONFIRMED |
| IF59 | KobIfdPair | update: keeperTip >= 0 (hostile field) | NIU6 | NIU6 | CONFIRMED |
| IF60 | KobIfdPair | update: waits for activeFrom | NIU7 | NIU7 | CONFIRMED |
| IF61 | KobIfdPair | upd 1 needs nb == 0: a stop fill run as an update (no custody spent, the matcher pays the B, the entry state shrinks below its UTXO) is refused. NI27 (an update tx with nb > 0, no exit) stays rejected by the exit checks. | NIU8; stay rejected: NI27 | NIU8 | CONFIRMED |
| IF62 | KobIfdPair | refund: refundTip >= 0 (hostile field: the keeper would top up the maker) | NIR1 | NIR1 | CONFIRMED |
| IF63 | KobIfdPair | refund: the A custody returns with its carrier | NIR2 | NIR2 | CONFIRMED |
| IF64 | KobIfdPair | refund: the B custody returns with its carrier | NIR3 | NIR3 | CONFIRMED |
| IF65 | KobIfdPair | refund: the KAS at output i is the maker's | NIR4 | NIR4 | CONFIRMED |
| IF66 | KobIfdPair | tplState: the input is a P2SH spend of the redeem script it carries (a planted non-P2SH UTXO of the evidence id with the genuine KobAsk bytes refused) | NE24 | NE24 | CONFIRMED |
| IF67 | KobIfdPair | tplState: the redeem script is the template (hash of prefix and suffix; a look-alike evidence with the genuine state refused) | NE23 | NE23 | CONFIRMED |
| IF68 | KobIfdPair | heldGx: the custody input carries the custody token (KCC/KCC: the B custody named at the A custody input of the same program, the real prefund left behind) | NIH4 | NIH4 | CONFIRMED |
| IF69r | KobIfdPair | heldGx KRON: the custody is owned by this entry (not another covenant) | NIH1r | NIH1r | CONFIRMED |
| IF70r | KobIfdPair | heldGx KRON: the custody owner is a covenant id (id_type 2); an id_type-3 custody is also refused by the KRON program (no key), so only the entry input flips | NIH2r | NIH2r(F) | CONFIRMED |
| IF71r | KobIfdPair | heldGx KRON: the custody is not a minter | NIH3r | NIH3r | CONFIRMED |
| IF72k | KobIfdPair | heldGx KCC-20: the custody is owned by this entry (not another covenant) | NIH1k | NIH1k | CONFIRMED |
| IF73k | KobIfdPair | heldGx KCC-20: the custody is covenant-owned (scheme 0x04) with borrowing disabled | NIH2k | NIH2k | CONFIRMED |
| IF74 | KobIfdPair | pinTok: the template source input carries the token (KCC/KCC: a B input as the A template source) | NIP1 | NIP1 | CONFIRMED |
| IF75 | KobIfdPair | pinTok: the template bytes read from the source hash to the token template (a planted look-alike source; the A program also refuses the output written on its bytes, so only the entry input flips) | NIP2 | NIP2(F) | CONFIRMED |
| IF76 | KobIfdPair | noStrays: at most MAX_TOK_IN (8) inputs of the token (the scan is unrolled for 8; a 16-input KCC-20 program carries a 9th) | NIS8 | NIS8 NIS9 | CONFIRMED |
| IF77s0 | KobIfdPair | noStrays: the input at token-input slot 0 is not owned by this entry | NIS0 | NIS0 | CONFIRMED |
| IF77s1 | KobIfdPair | noStrays: the input at token-input slot 1 is not owned by this entry | NIS1 | NIS1 NIS1(F) | CONFIRMED |
| IF77s2 | KobIfdPair | noStrays: the input at token-input slot 2 is not owned by this entry | NIS2 | NIS2 NIS2(F) | CONFIRMED |
| IF77s3 | KobIfdPair | noStrays: the input at token-input slot 3 is not owned by this entry | NIS3 | NIS3 NIS3(F) | CONFIRMED |
| IF77s4 | KobIfdPair | noStrays: the input at token-input slot 4 is not owned by this entry | NIS4 | NIS4 | CONFIRMED |
| IF77s5 | KobIfdPair | noStrays: the input at token-input slot 5 is not owned by this entry | NIS5 | NIS5 | CONFIRMED |
| IF77s6 | KobIfdPair | noStrays: the input at token-input slot 6 is not owned by this entry | NIS6 | NIS6 | CONFIRMED |
| IF77s7 | KobIfdPair | noStrays: the input at token-input slot 7 is not owned by this entry | NIS7 | NIS7 | CONFIRMED |
| IF85 | KobIfdPair | rd: the evidence sigscript starts with the 8-byte fill push (a cancel whose signature reads as a fill refused; an index at a P2PK input also fails the template read) | NE03a0, NE03b0, NE03a1, NE03b1; stay rejected: NE16a0, NE16b0, NE16a1, NE16b1 | NE03b0 NE03b1 NE03a0 NE03a1 | CONFIRMED |
| IF86 | KobIfdPair | rd: the evidence is filled (n > 0; an entry with minTouch 0 would otherwise arm on an unfilled order: the n = 0 push is its refund / update encoding, which the evidence order itself refuses here before expiry, so only the entry input flips) | NE01a0, NE01b0, NE01a1, NE01b1 | NE01b0(F) NE01b1(F) NE01a0(F) NE01a1(F) | CONFIRMED |
| IF87 | KobIfdPair | rd: the A / pair evidence fills at least minTouch | NE07a0, NE07b0, NE07a1, NE07b1 | NE07b0 NE07b1 NE07a0 NE07a1 | CONFIRMED |
| IF88 | KobIfdPair | rd: the evidence's token and scale (mode 1: side, S, T and both scales) are the entry's. NE13 (the opposite pair order) stays rejected: its custody is the other token (the tk read) (the edited evidence order also refuses its doctored state by its own script, so only the entry input flips) | NE09a0, NE09b0, NE09a1, NE09b1, NE11a0, NE11b0, NE11a1, NE11b1; stay rejected: NE13a1, NE13b1 | NE09b0(F) NE11b0(F) NE09b1(F) NE11b1(F) NE09a0(F) NE11a0(F) NE09a1(F) NE11a1(F) | CONFIRMED |
| IF89 | KobIfdPair | rd: the evidence is not decaying (slope 0) (the edited evidence order also refuses its doctored state by its own script, so only the entry input flips) | NE05a0, NE05b0, NE05a1, NE05b1 | NE05b0(F) NE05b1(F) NE05a0 NE05a1(F) | CONFIRMED |
| IF90 | KobIfdPair | rd: the ask / pair evidence's custody tk carries its S token (KCC / KCC: an old UTXO of the other token owned by the evidence, marker right; the pair evidence's own stray guard refuses it at its input). On the mixed pairs NE18 stays rejected: the other token is of another program, so the holder template check (IF116) refuses it too | NE18ka0, NE18ka1, NE18kb1; stay rejected: NE18a0, NE18a1, NE18b1 | NE18kb1(F) NE18ka0 NE18ka1(F) | CONFIRMED |
| IF91 | KobIfdPair | rd: tk is owned by the evidence order with the covenant marker | NE19a0, NE19a1, NE19b1 | NE19b1 NE19a0 NE19a1 | CONFIRMED |
| IF116 | KobIfdPair | rd: the ask / pair evidence's custody tk is a holder of its token's program (template hash; P2SH kept): an old look-alike holder of another program refused | NE25a0, NE25a1, NE25b1 | NE25b1(F) NE25a0(F) NE25a1(F) | CONFIRMED |
| IF117 | KobIfdPair | rd: tk is the P2SH of the holder redeem it shows (template hash kept): an old non-P2SH input with the genuine redeem refused | NE26a0, NE26a1, NE26b1 | NE26b1(F) NE26a0(F) NE26a1(F) | CONFIRMED |
| IF92 | KobIfdPair | evLeg: the B evidence sigscript starts with the 8-byte fill push (a cancel read as a fill refused) | NE04a0, NE04b0 | NE04b0 NE04a0 | CONFIRMED |
| IF93 | KobIfdPair | evLeg: the B evidence is filled (n > 0; minTouch 0 makes the B threshold 0; the unfilled order itself refuses its n = 0 push here, so only the entry input flips) | NE02a0, NE02b0 | NE02b0(F) NE02a0(F) | CONFIRMED |
| IF94 | KobIfdPair | evLeg: the B evidence fills at least ceil(minTouch * entryStop / scale(A)) | NE08a0, NE08b0 | NE08b0 NE08a0 | CONFIRMED |
| IF95 | KobIfdPair | evLeg: the B evidence is of token B (the edited evidence order also refuses its doctored state by its own script, so only the entry input flips) | NE12a0, NE12b0 | NE12b0(F) NE12a0(F) | CONFIRMED |
| IF96 | KobIfdPair | evLeg: the B evidence quotes per B's scale (the edited evidence order also refuses its doctored state by its own script, so only the entry input flips) | NE10a0, NE10b0 | NE10b0(F) NE10a0(F) | CONFIRMED |
| IF97 | KobIfdPair | evLeg: the B evidence is not decaying (the edited evidence order also refuses its doctored state by its own script, so only the entry input flips) | NE06a0, NE06b0 | NE06b0 NE06a0(F) | CONFIRMED |
| IF98 | KobIfdPair | evLeg: the B ask evidence's custody tk carries token B (KCC / KCC: an old UTXO of A owned by the B ask). On the mixed pairs NE18b0 stays rejected by the holder template check (IF118) | NE18kb0; stay rejected: NE18b0 | NE18kb0 | CONFIRMED |
| IF99 | KobIfdPair | evLeg: tk is owned by the B ask evidence with the covenant marker | NE19b0 | NE19b0 | CONFIRMED |
| IF118 | KobIfdPair | evLeg: the B ask evidence's custody tk is a holder of token B's program (template hash; P2SH kept): an old look-alike holder of another program refused | NE25b0 | NE25b0(F) | CONFIRMED |
| IF119 | KobIfdPair | evLeg: tk is the P2SH of the holder redeem it shows (template hash kept): an old non-P2SH input with the genuine redeem refused | NE26b0 | NE26b0(F) | CONFIRMED |
| IF100 | KobIfdPair | stopTouch: evMode is 0 or 1 | NE21a1, NE21b1 | NE21b1 NE21a1 | CONFIRMED |
| IF101 | KobIfdPair | stopTouch: the B quote is positive (an ask of B at price 0 would arm any buy stop: ceil(stop * 0 / scale(B)) = 0) | NE22b0 | NE22b0 | CONFIRMED |
| IF102 | KobIfdPair | stopTouch: a sell stop arms only at a rate <= entryStop (one unit beyond refused; PE17 exactly at the stop accepted) | NE17a0, NE17a1 | NE17a0 NE17a1 | CONFIRMED |
| IF103 | KobIfdPair | stopTouch: a buy stop arms only at a rate >= entryStop | NE17b0, NE17b1 | NE17b0 NE17b1 | CONFIRMED |
| IF104 | KobIfdPair | merge: m > 0 (a merge never reads the exit refund / update push n = 0 as an amount). NIM9 (m = 0 next to a take-profit of n) stays rejected by the push check IF110 | NIM10; stay rejected: NIM9 | NIM10 | CONFIRMED |
| IF104c | KobIfdPair | NIM9 has the exit at input 1 (at input 0 a merge of 0 is the refund encoding 0). m > 0 AND the entry / exit push cross-checks removed: a merge of 0 next to a take-profit of n leaves the n bought A and back(n) to the matcher | NIM9 | NIX7 NIX7e NIM9 | CONFIRMED |
| IF105 | KobIfdPair | merge: the exit at k carries the committed exit state (outside the mutable payloads): a look-alike with another tpPrice refused | NIX1 | NIX1 | CONFIRMED |
| IF106 | KobIfdPair | merge: the exit is booked by this entry (push opcodes, parent, rptPrice = price, rptPre): another rptPrice refused | NIX2 | NIX2 | CONFIRMED |
| IF107 | KobIfdPair | merge: the committed exit is the entry's opposite side (KCC / KCC: a buy-first entry whose committed exit is a BID refused). On the mixed pairs NIX3m stays rejected by the exit's sExt check (B's commitment is not A's; IF107x / IF107c) | NIX3; stay rejected: NIX3m | NIX3 | CONFIRMED |
| IF107x | KobIfdPair | merge: the exit's sExt is the commitment of its custody token as fill writes it (aExt buy-first, bExt sell-first). DEFENCE IN DEPTH on NIX3m with the side check IF107; IF107c removes both | stay rejected: NIX3m |  | held (defence in depth) |
| IF107c | KobIfdPair | the committed exit's side AND its sExt check removed: a buy-first entry merges a BID exit (mixed pairs: its sExt is B's commitment) | NIX3m | NIX3 NIX3m | CONFIRMED |
| IF108 | KobIfdPair | merge: the committed exit quotes at the entry's scale of A (an exit at scale 999 refused: the two would round the merge differently) | NIX4 | NIX4 | CONFIRMED |
| IF109 | KobIfdPair | merge: the exit's sigscript starts with an 8-byte push (a 9-byte push whose bytes [1..9) are m refused; the exit's own byte[8] argument refuses it too). NIX6 (a minimal push) stays rejected by the amount check | NIX5; stay rejected: NIX6 | NIX5(F) | CONFIRMED |
| IF110 | KobIfdPair | merge: the exit's pushed n is m. DEFENCE IN DEPTH with the exit's own check of the entry's push (KobCondPair, cond-pair-rpt CR7): an entry merging m != n stays rejected by the exit (NIX7, judged at the exit); IF110c removes both | stay rejected: NIX7 | NIX7e(F) | held (defence in depth) |
| IF110c | KobIfdPair | the entry's check of the exit's push AND the exit's check of the entry's push removed: an entry merging m = n - 1 leaves budget(1 whole A) and the exit's KAS to the matcher | NIX7, NIX7e | NIX7 NIX7e | CONFIRMED |
| IF111 | KobIfdPair | merge: the buy-first budget(m) is positive (a price-0 entry re-armed with no escrow growth refused) | NIM8 | NIM8 | CONFIRMED |
| IF112 | KobIfdPair | merge into an existing A custody: it keeps its carrier | NIM6 | NIM6 | CONFIRMED |
| IF113 | KobIfdPair | merge into an existing B custody: it keeps its carrier | NIM7 | NIM7 | CONFIRMED |
| IF114 | KobIfdPair | merge sell-out: the exit custody input xc is authenticated (heldGx): xc named at a P2PK input would shrink the entry's claim of the exit's KAS | NIX11 | NIX11 | CONFIRMED |
| IF115 | KobIfdPair | a booking needs n < 2^53 (the merge argument): a KCC-20 A booked for 2^53 refused (PIF12k books 2^53 - 1) | NIF12k | NIF12k | CONFIRMED |

Collateral flips (scenarios accepted besides the expected ones):

- IF9b: NI19
- IF8c: NIO9
- IF76: NIS9
- IF104c: NIX7 NIX7e
- IF107c: NIX3
- IF110: NIX7e

## cond-pair-rpt

KobCondPair repeat / merge (exit of KobIfdPair); test binary `kob_ifd_pair_tests`; contracts `contracts/v2`. 10/10 confirmed, 2/2 defence-in-depth held (expected NOT to flip).
Baseline (no mutation): cond_rpt_until clean, cond_rpt_profit clean, cond_rpt_merge_k clean, cond_rpt_rules clean, ifd_merge_rules clean.

| id | contract | check ablated | expected | flipped | status |
|---|---|---|---|---|---|
| CR1 | KobCondPair | a re-arming take-profit pays the maker exactly its profit dOut at output i (ASK exit: tOut - budget). Removing the maker-delivery pin lets the re-arm skim the profit. | NCR1 | NCR1 | CONFIRMED |
| CR2 | KobCondPair | a booked exit (parent set) takes profit on leg 0 without its entry present only once tx.daa >= rptUntil; before that it must run together with its entry merge | NCRU | NCRU | CONFIRMED |
| CR3 | KobCondPair | update: a booked exit is never armed next to its repeat entry (here: next to a fill of the entry) | NCR4 | NCR4 | CONFIRMED |
| CR4 | KobCondPair | the stop leg never re-arms: a booked exit's stop fill is refused next to its entry (here its merge: the matcher would fund a re-arm after a stop-loss) | NCR5 | NCR5 | CONFIRMED |
| CR6 | KobCondPair | the entry's sigscript starts with an 8-byte push (a 9-byte push carrying the merge argument refused; the entry's own byte[8] argument refuses it too) | NCR9 | NCR9(F) | CONFIRMED |
| CR7 | KobCondPair | the entry's pushed merge argument names this exit and its n. DEFENCE IN DEPTH with the entry's check of this exit's push (KobIfdPair IF110): an entry merging m != n stays rejected by the entry (NIX7e); ifd-pair IF110c removes both | stay rejected: NIX7e | NIX7(F) | held (defence in depth) |
| CR8 | KobCondPair | re-arm: the maker profit is positive (a committed take-profit at the entry price gives 0: refused; a 0-amount token output is also refused by the KRON program, so on KCC/KRON only the exit input flips) | NCR6 | NCR6(F) NCR6 | CONFIRMED |
| CR9 | KobCondPair | a sold-out re-arm leaves no S: DEFENCE IN DEPTH with the S-output pin (outIdx = output i, where the profit output sits: the two pins collide). CR9c removes both | stay rejected: NCR7 |  | held (defence in depth) |
| CR9c | KobCondPair | outAmount == 0 AND the S-output pins (script, covenant) removed: a look-alike exit holding one A more than its amountLeft sells out and the extra A goes to the matcher | NCR7 | NCR7 | CONFIRMED |
| CR10 | KobCondPair | a sold-out re-arm: the maker's profit output carries deliveryCarrier (the rest of the exit's KAS is the entry's) | NCR8 | NCR8 | CONFIRMED |
| CR11 | KobCondPair | the exit's S stray guard keeps the entry's xc honest: xc named at a genuine A UTXO owned by the exit (with one sompi of KAS) passes the entry's heldGx, only the exit refuses it as a stray (duplicate of cond-pair G1, witnessed by a merge) | NIX9 | NIX9 | CONFIRMED |
| CR12 | KobCondPair | re-arm: n < MERGE_K. A look-alike exit (parent = the entry, attacker-funded, KCC-20 A) trading n1 = 2^53 + m at input i reads the entry's merge of the booked exit at i + 1 for m, -((i + 1) * 2^53 + m), as its own -(i * 2^53 + n1) and re-arms without the entry crediting it | NCR10 | NCR10 | CONFIRMED |

Collateral flips (scenarios accepted besides the expected ones):

- CR7: NIX7
