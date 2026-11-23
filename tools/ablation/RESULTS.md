# Ablation results

## Full run (2026-11-24, audit AUD-COV): every require has an entry or a documented reason

Catalogs and harness of `a7e9d9ef2` (the executables of the full run were built from it), the re-run of three repaired
entries from `ed2f83f70`. Contracts unchanged (the templates of the 2026-11-23 release candidate).

**Coverage** (`node tools/ablation/coverage.mjs`, 1,337 `require` lines): **99 -> 46 not mutated**, and the 46 are all checks
documented in their catalog header as having no witness attack:

| Template | before | after | the lines left |
|---|---|---|---|
| KobAsk / KobAskKron | 2 / 2 | 2 / 2 | `sb.length == 65`, `refundTip >= 0` |
| KobBid / KobBidKron | 5 / 2 | 5 / 2 | as KobAsk; KobBid also `topupTip >= 0`, the top-up `have >= 0`, isCustody's P2SH line (documented in kcc20-sell) |
| KobCondAsk / KobCondAskKron | 5 / 5 | 3 / 3 | `sb.length`, `refundTip`, the update side check after `touch()` |
| KobCondBid / KobCondBidKron | 10 / 5 | 4 / 3 | as KobCondAsk; KobCondBid also the top-up `have >= 0` (now documented in kcc20-buy) |
| KobIfdAsk / KobIfdAskKron | 4 / 4 | 3 / 3 | `sb.length`, two `refundTip` |
| KobIfdBid / KobIfdBidKron | 3 / 3 | 2 / 2 | `sb.length`, `refundTip` |
| KobPair | 16 | 2 | `sb.length`, the top-up `have >= 0` (now documented in pair) |
| KobCondPair | 25 | 4 | `sb.length`, `refundTip`, `tScale > 0`, the top-up `have >= 0` (now documented in cond-pair) |
| KobIfdPair | 5 | 3 | `sb.length`, `bScale > 0`, the fill branch `n > 0` |
| KobCurveAsk | 3 | 3 | `sb.length`, `levelSize > 0`, `n <= amountLeft` (documented in curve-ask) |
| **total** | **99** | **46** | |

**New entries (53 lines)**, all confirmed or held in the run below:

* Evidence interval sign `require(iv >= 0)` (audit 2026-10-29 F1, 16 lines): `lib/touch.mjs` check `ivpos`, generated as
  `TA` / `TU` / `TI` / `TB` / `TV` / `TK-ivpos` in the four KAS-kind catalogs, witnessed by the touch batteries' 04n2 / 04n3
  (interval -R / -10^12 on an evidence UTXO one DAA old; 04n1 is also refused by another input). Pairs: cond-pair `EVIV` (rd:
  new NC5V, NC3Va / b) and `EVBIV` (evLeg: new NC2Va / b); ifd-pair `IF125` (rd: new NE27a0 / b0 / a1 / b1) and `IF126`
  (evLeg: new NE28a0 / b0). The edited evidence order refuses its own doctored state, so the pair ones are `inputOnly`.
* KobCondBid (kcc20-buy): `DM-CB-iscust-p2sh` (DM3-05, input-only: a planted non-P2SH custody), `DM-CB-iscust-state`
  (DM3-02..04, DM6-07), `DM-CB-refund-pin` (DM6-01..03), `DM-CB-refund-cov` (new DM6-10: an unbound copy of the release).
* KobPair (pair `DM-P12`..`DM-P25`) and KobCondPair (cond-pair `DM-CP4`..`DM-CP22`), the delivery modes, witnessed by the
  shared `pair_harness::mode_battery` / `kron_t_battery`: mode range, KRON T mode 0 (NM91 / NM92), `bought >= 0` (new
  NM67), `bought == 0` (new NM52b / NM53b: the custody spent by a mode-0 / mode-2 order; NM52 / NM53 held), the receive
  fields (NM62, NM64, NM65, NM77, NM86, NM87), `form == 2` (new NM63b; NM63 held), the top-up receive state (NM70, NM71,
  NM72; NM73 held: the commitment is also pinned on output i and kept by the token program), the bought-custody's P2SH (NM08,
  input-only on the public-mint / KaspaRocket mixes) and state (cond-pair, NM01..07, NM19), the refund release's KAS (NM42).
  `DM-P12` / `DM-CP4` (the bought-custody's `tFamily == FAM_KCC20`) are holds on the new NM93 (a KRON T held as a custody):
  the custody's KCC-20 state equality refuses it too. `DM-CP21` / `DM-CP22`: the BID exit's S profit pins under dS, new
  NCR1b / NCR1c (sell-first re-arms in `kob_ifd_pair_tests::cond_rpt_profit`).
* Harness fix: `pair_harness::dlv_block` wrote the delivery fields as two's complement; the contract reads sign-magnitude, so
  a negative field read as another negative number (NM67 could not be built). It now writes sign-magnitude.

**Full run** (every entry of every suite, `--jobs 2`, debug executables pinned in a private `--exe-dir`, one suite per
invocation, baselines clean in every run; KCC-20 3 / 3 program of the KAS-kind suites: KCC20Ref; the pair suites on their
8/8, 3/3, public-mint and KaspaRocket mixes):

| Suite | Entries | Confirmed | Held | Not confirmed in the run | Wall time |
|---|---|---|---|---|---|
| `kcc20-sell` | 347 (+ DM-9x) | 336/338 | 9/9 | AU-B-4, DM-9 (repaired, see below) | 4,671 s |
| `kcc20-buy` | 263 | 255/255 | 8/8 | | 8,515 s |
| `kron-sell` | 326 | 319/320 | 6/6 | AU-KB-4 (repaired) | 3,466 s |
| `kron-buy` | 221 | 213/213 | 8/8 | | 6,880 s |
| `pair` | 104 | 101/101 | 3/3 | | 200 s |
| `cond-pair` | 156 | 145/145 | 11/11 | | 325 s |
| `cond-pair-rpt` | 12 | 10/10 | 2/2 | | 129 s |
| `ifd-pair` | 137 | 133/133 | 4/4 | | 1,052 s |
| `curve-ask` | 44 | 44/44 | 0 | | 51 s |
| **total** | **1,610 (+1)** | **1,556/1,559** | **51/51** | **3** | **25,289 s** |

No `EDIT-ERROR` or `BASELINE-DIRTY`. The three entries the run did not confirm had lost their witness to a later contract
change that put a second check in front of it; no check was missing and no attack was accepted by the unmutated contracts:

* `AU-B-4` / `AU-KB-4` (fill `n > 0` of KobBid / KobBidKron): since the minimum-fill rule of 2026-11-18 (`71a6c32fe`) a fill
  below minFill must take all the buying power left, and NAU-B03 / NAU-KB03 (n = 0 on a bid with 1 KAS spare) were refused by
  that rule too. The scenarios now use a bid whose spare (0.001 KAS) buys less than one base unit (its remainder, below
  REFUND_MIN, stays on the delivery). Re-run: both confirmed.
* `DM-9` (KobBid top-up: the receive UTXO is the P2SH of its script): since DLV-07 the non-P2SH look-alike of NT10 authorises no
  output, so the successor check `OpAuthOutputIdx(tokIn, 0) == self` refuses it too. `DM-9` is now a hold, new `DM-9x` removes
  both and flips NT10. Re-run: held / confirmed.

After the re-run: **1,559/1,559 confirmed, 52/52 defence-in-depth held** over the nine suites. The detail tables of all nine
suites below are this run's (the three repaired rows from the re-run).

## Release candidate dry run (2026-11-23, audit AUD-COV)

On the templates of the 2026-11-23 release candidate (after the delivery, DLV-07, H1, minimum-fill and curve re-pins), no
cargo: `node tools/ablation/ablate.mjs --dry-run --suite all`: 1,557 edits applied, **0 edit errors** (18 s). `node
tools/ablation/coverage.mjs`: of 1,337 `require` lines, **99 are not mutated** by any catalog entry (35 on 2026-10-13, all
documented then). Most new ones are the November delivery-mode checks: KobCondPair 25, KobPair 16, KobCondBid 10, KobBid 5,
KobCondAsk 5, KobCondAskKron 5, KobCondBidKron 5, KobIfdPair 5, KobIfdAsk 4, KobIfdAskKron 4, KobCurveAsk 3, KobIfdBid 3,
KobIfdBidKron 3, KobAsk 2, KobAskKron 2, KobBidKron 2. They are neither witnessed nor documented yet. No mutation was run
(the full run is cargo test executables, about 2 hours at `--jobs 2` by the Runtime section of README.md).

## KaspaRocket's program (2026-10-27, its mainnet review)

`kasparocket-token-11458` (`KaspaRocketToken11458`: the public-mint `KCC20` actor at 10 / 10 slots, reviewed 2026-10-27) was run
through every KCC-20 and pair catalog: the KAS-kind suites with `KOB_KCC20_PROGRAM=KaspaRocketToken11458` (every test fn but the
8/8 and `_pm` ones), the pair suites with their permanent KaspaRocket mixes (`pair_harness::mixed_pairs_kr`, `KKKR`, the
`audit_pair_sec_tests` reproducers and NIS8 / NIS9 on it). `lib/parse.mjs` now also reads `KaspaRocketToken` / `KForgeToken`
program names in the verdict lines (they were not recorded before).

Runs: debug executables built from the branch (pinned in a private `--exe-dir`), `--jobs 4`, baselines clean in every run:

| Suites | Program | Entries | Confirmed | Held | Seen on KaspaRocket's program | Wall time |
|---|---|---|---|---|---|---|
| `kcc20-sell` | `KOB_KCC20_PROGRAM=KaspaRocketToken11458` | 301 | 295 / 295 | 6 / 6 | 297 | 1,668 s |
| `kcc20-buy` | `KOB_KCC20_PROGRAM=KaspaRocketToken11458` | 222 | 214 / 214 | 8 / 8 | 220 | 2,679 s |
| `pair`, `cond-pair`, `cond-pair-rpt`, `ifd-pair` | 8/8, 3/3, public-mint and KaspaRocket mixes | 362 | 344 / 344 | 18 / 18 | 360 | 595 s |

No `NOT-CONFIRMED`, `EDIT-ERROR` or `BASELINE-DIRTY`. Completion (`coverage.mjs --suite kcc20-sell,kcc20-buy,pair,cond-pair,cond-pair-rpt,ifd-pair
--results <the three result files> --program KaspaRocketToken11458`): of the 758 `require` lines of the nine KCC-20 and pair templates, 30 are
not witnessed on KaspaRocket's program: the 22 documented lines of the static check (`sb.length == 65`, `refundTip >= 0`, the side checks after
`touch()`, `tScale > 0` / `bScale > 0`, KobIfdPair's fill `n > 0`); the scan bound `cnt <= MAX_TOK_IN` of the six KAS kinds (6: their
nine-input witnesses run on the 8/8 program, where the order is the only input that flips; on KaspaRocket's program the bound is witnessed in
the pair suites, KobIfdPair NIS8, and by `kcc20_public_mint_tests` at the builders); KobIfdPair's `OpInputCovenantId(idx) == tok` /
`OpInputCovenantId(src) == tok` (2: their KCC / KCC witnesses NIH4 / NIP1 run on the 8/8 program). Unlike a 3 / 3 program, the stray-scan
slots 3..7 are witnessed on it.

## Public-mint program (2026-10-13, audit AUD-PM)

`kcc20-ref-public-mint` (`KCC20PublicMint`, the published build of the KCC-20 reference, 3 / 3, tradable since `41b7fdb`) was
run through every KCC-20 and pair catalog. Harness (`f7748cd`, `4677f31`): the KAS-kind suites' default token is the pinned
public-mint template under `KOB_KCC20_PROGRAM=KCC20PublicMint` (every test fn but the 8/8 ones); the touch batteries have
permanent `_pm` twins (slots gate as for `_3x3`); the audit stray-scan scenarios also run on the 3 / 3 token of the run at the
slots it takes; the pair suites (KobPair now included, which never ran on a 3 / 3 program before) run the public-mint build
permanently next to 8/8 and 3/3. Every verdict line names its program, the results record the programs each entry was seen
on, and `coverage.mjs --results ... --program KCC20PublicMint` counts only the entries confirmed or held on that program.

Runs: debug executables built from `4677f31` (pinned in a private `--exe-dir`), `--jobs 3`, baselines clean in every run:

| Suites | Program | Entries | Confirmed | Held | Seen on the public-mint build | Wall time |
|---|---|---|---|---|---|---|
| `kcc20-sell` | `KOB_KCC20_PROGRAM=KCC20PublicMint` | 297 | 291 / 291 | 6 / 6 | 273 | 1,979 s |
| `kcc20-buy` | `KOB_KCC20_PROGRAM=KCC20PublicMint` | 218 | 210 / 210 | 8 / 8 | 206 | 3,261 s |
| `pair`, `cond-pair`, `cond-pair-rpt`, `ifd-pair` | 8/8, 3/3 and public-mint mixes | 352 | 334 / 334 | 18 / 18 | 329 | 578 s |
| `kcc20-sell` / `kcc20-buy`, every entry with a 3 / 3 run (`--only`) | reference 3 / 3 | 89 / 73 | 88 / 88 / 72 / 72 | 1 / 1 / 1 / 1 | | 1,153 / 1,867 s |

No `NOT-CONFIRMED`, `EDIT-ERROR` or `BASELINE-DIRTY`. Completion (`coverage.mjs --suite kcc20-sell,kcc20-buy,pair,cond-pair,cond-pair-rpt,ifd-pair
--results <the three result files> --program KCC20PublicMint`): of the 742 `require` lines of the nine KCC-20 and pair
templates, 78 are not witnessed on the public-mint build, and each is one of

* the documented lines of the static check (20: `sb.length == 65`, `refundTip >= 0`, the side checks after `touch()`,
  KobIfdPair's fill `n > 0`);
* a stray-scan slot 3..7 or the scan bound `cnt <= MAX_TOK_IN` (53 lines over the nine templates): a fourth token input of one
  covenant does not exist on a 3 / 3 program (the token program refuses the transaction whatever the orders do);
* a check whose only attack needs a fourth token output of one covenant (5, the 3 / 3 reference has the same gap):
  KobCondPair `OpInputCovenantId(tk) == tok` / `== cTok` (NC59a / b, the evidence custody of another KCC-20 token), the
  re-arm merge's `OpCovInputCount(parent) == 0` (NCR4), KobIfdPair's committed-exit side and `sExt` checks (NIX3 / NIX3m,
  the `pair.rearm.ask` shape of AUD-33).

## Audit completion (2026-10-13): every require of the KAS kinds has an entry

Pre-mainnet internal audit. The KAS-kind catalogs left most `require` lines of the twelve KAS-kind templates (KobAsk,
KobBid, KobCondAsk, KobCondBid, KobIfdAsk, KobIfdBid and the KRON twins) without a mutation, the core checks of KobAsk and
KobBid included (payout floors, cancel signature, refund rules, custody identity, the stray scans). Every one of them now
has an entry: a confirmed flip, or a held defence-in-depth entry plus a combined entry that flips, or it is listed with
its reason in the catalog header as a check without a witness attack. New tool `coverage.mjs` (README, "Coverage")
measures this with an exact line diff; the earlier ad-hoc measure (a prefix / suffix diff) counted every require lying
between two edits of a multi-edit entry as covered (KobCondAsk's whole stray scan, for example).

`require` lines without any mutation (`node tools/ablation/coverage.mjs --summary`; before = the catalogs of `cda2cb4`):

| Template | require lines | before | after |
|---|---|---|---|
| KobAsk / KobAskKron | 51 / 47 | 47 / 39 | 2 / 2 |
| KobBid / KobBidKron | 46 / 42 | 43 / 38 | 2 / 2 |
| KobCondAsk / KobCondAskKron | 91 / 87 | 55 / 47 | 3 / 3 |
| KobCondBid / KobCondBidKron | 86 / 82 | 55 / 46 | 3 / 3 |
| KobIfdAsk / KobIfdAskKron | 91 / 87 | 64 / 52 | 3 / 3 |
| KobIfdBid / KobIfdBidKron | 77 / 72 | 57 / 46 | 2 / 2 |
| KobPair / KobCondPair / KobIfdPair | 71 / 116 / 113 | 2 / 3 / 4 | 1 / 2 / 2 |
| **total** | **1,159** | **598** | **35** |

The 35 left are documented in the catalog headers, none can make a transaction acceptable: `sb.length == 65` before
checkSig (15: every cancel), `refundTip >= 0` (15: it only raises what the maker must receive), the touch side checks of
the four conditional updates (4: `touch()` returns one of the two sides), and KobIfdPair's fill-branch `n > 0` (1).

Entries added (`AU-*`; scenarios `NAU-*`, positive controls `PAU-*`, in new `*audit*` test fns, the existing baselines
untouched):

| Suite | New entries | Test fns | Scenarios |
|---|---|---|---|
| `kcc20-sell` | 196 (KobAsk 46, KobBid 41, KobCondAsk 53, KobIfdBid 56) | `v3_audit_ask`, `v3_audit_bid`, `v3_audit_cond_ask`, `v3_audit_cond_ask_strays`, `v3_audit_ifd_bid` | NAU-A01..A22, AS0..7; B01..B26, BS0..7; CA01..CA39, CAS0..7; IB01..IB35, IBS0..7 |
| `kcc20-buy` | 117 (KobCondBid 53, KobIfdAsk 64) | `v3_buy_audit_cond_bid`, `v3_buy_audit_ifd_ask` | NAU-CB01..CB31, CBS0..7; IA01..IA39, IAS0..7 |
| `kron-sell` | 164 (KobAskKron 38, KobBidKron 36, KobCondAskKron 45, KobIfdBidKron 45) | `v3_audit_{ask,bid,cond_ask,ifd_bid}_kron` | NAU-KA, KB, KCA, KIB (+ KAS / KBS / KCAS / KIBS 0..3) |
| `kron-buy` | 95 (KobCondBidKron 45, KobIfdAskKron 50) | `kron_v2_buy_audit_cond_bid`, `kron_v2_buy_audit_ifd_ask` | NAU-KCB01..KCB25, KCBS0..3; KIA01..KIA35, KIAS0..3 |
| `pair` / `cond-pair` / `ifd-pair` | 1 / 1 / 2 | `pair_custody`, `cond_pair_custody`, `ifd_refund`; `audit_pair_sec_tests::aud_rb_ifd_pair_rearm_bound` | NP28sk, NC107sk, NIH5k; NIAUD-RB1 / RB2 (IF-AUD-RB) |

Witness classes (the KRON scenarios mirror the KCC-20 ones, adapted to the KRON encoding and its 4-input limit):

* **Custody identity** (every custody-holding order): a sibling UTXO of the order's covenant id beside it
  (`OpCovInputCount(selfId) == 1`), a custody of another token of the same program, planted custodies of the token's
  covenant id (not P2SH; P2SH of a look-alike template built around the genuine 112-byte state, so only the template hash
  refuses it), tokens of another owner, borrowing enabled, another extension commitment (KCC-20; also the pair orders'
  `sExt` / `ext`, which only `kob_pair_custody_ext_tests` asserted before).
* **Outputs**: a second output bound to the order's id (rest, termination, refund, update: a keeper or taker would own
  a UTXO of the id, and with it every later stray), continuations re-priced or handed to the taker, carriers of the rest
  custody / returned rest / delivery skimmed, the rest or delivery left unbound (a copy of the holder script while the
  taker keeps the tokens), deliveries owned by the seller, the payout slot paid to the taker, refunds paid to the keeper or
  short, sold-out payouts one sompi short, the IFD exit built from a look-alike template (anyone could spend the exit and
  the tokens it owns).
* **Time and fill size**: fills / updates before activeFrom, refunds before the idle bound, auction times above the
  lockTime or before their origin, n > amountLeft (on a plain ask only an evidence fill claiming more than it sells can use
  it: NAU-A09 arms a stop of minTouch 1000 with a 1-base-unit ask), a zero fill closing an exhausted bid or booking an empty
  exit, re-arm and merge rules (merge of an exit whose terms differ in the windows [189..244) / [252..271) or in its
  extension commitment, the repeat bound of an unbooked fill).
* **Hostile fields** (maker-set parameters the builders refuse; witnessed as the existing NV7 and the pair suite's
  C9s / C9t): negative scale, tip, reserve, delivery / exit carrier, slope, decayStep, slipBps outside 0..10000, stopPrice
  above MAX_STOP, negative keeperTip (as M4 / NK2), a stop-only order filled on the TP leg, an armed order without a stop leg.
* **Stray scans**: one entry per unrolled slot line (a stray of the order at token slot k: 8 slots on the 8/8 KCC-20
  token, 4 on KRON) plus the bound (a 9th / 5th token input, `inputOnly`: the token program refuses it on its own input).

Defence in depth (hold + combined entry): `n > 0` and the minFill rule of the asks and conditional sells (AU-A-13 / 13x,
AU-CA-15 / 15x and the KRON twins), the IFD exit script equality and its genesis id (AU-IB-20 / 20x, AU-KIB-18 / 18x; the
covenant id is derived from the script), the exit template hash beside NI4 (AU-IB-19, AU-KIB-17 hold NI4), the conditional
buy's update `scale > 0` and the evidence ask's own `scale > 0` (AU-CB-34 / 34x, AU-KCB-39 / 39x, judged at the evidence
input), and the cross-contract pairs of the buy side (AU-IA-13 / 13x, 14 / 14x, 28 / 28x; AU-KCB-5 / 5x, AU-KIA-12 / 12x).

Also: duplicate scenario ids made unique (`NT-IOC2`, KCC-20 and KRON; `NT-FOK2`, KRON), harness fixture fields with
defaults that change nothing (BidP `reserve` / `dc`, IfdP `scale` / `active_from` / `dc` / `ec`, CondP `ext`, Ev `claim`,
IfdKnobs `edit`, and the buy / KRON equivalents).

Runs: debug executables, `--exe-dir`, `--jobs 2`, one suite per invocation, baselines clean in every run. Final
consolidated runs of every new entry (scratch `--out` directories):

| Suite | Command (`--only` = every `AU-*` id of the catalog, or the ids named) | Totals line | Wall time |
|---|---|---|---|
| `kcc20-sell` | `ablate.mjs --suite kcc20-sell --only <196 AU-*> --jobs 2 --exe-dir <bin>` | 193/193 confirmed, 3/3 defence-in-depth held | 1,108 s |
| `kcc20-buy` | `ablate.mjs --suite kcc20-buy --only <117 AU-*> --jobs 2 --exe-dir <bin>` | 113/113 confirmed, 4/4 defence-in-depth held | 1,786 s |
| `kron-sell` | `ablate.mjs --suite kron-sell --only <164 AU-*> --jobs 2 --exe-dir <bin>` | 161/161 confirmed, 3/3 defence-in-depth held | 1,392 s |
| `kron-buy` | `ablate.mjs --suite kron-buy --only <95 AU-*> --jobs 2 --exe-dir <bin>` | 91/91 confirmed, 4/4 defence-in-depth held | 2,538 s |
| `pair` | `ablate.mjs --suite pair --only C10k --jobs 2 --exe-dir <bin>` | 1/1 confirmed | 2 s |
| `cond-pair` | `ablate.mjs --suite cond-pair --only C10k --jobs 2 --exe-dir <bin>` | 1/1 confirmed | 3 s |
| `ifd-pair` | `ablate.mjs --suite ifd-pair --only IF73x,IF-AUD-RB --jobs 2 --exe-dir <bin>` | 2/2 confirmed | 2 s |

**562/562 confirmed, 14/14 held**, no `NOT-CONFIRMED`, `EDIT-ERROR` or `BASELINE-DIRTY` (`--dry-run --suite all`: 1,388 edits
applied, 0 edit errors). The baselines of every test fn these entries name (75, the existing touch batteries and negative suites
included) were clean. The existing entries were not re-run: no contract changed, and the harness edits only add
scenarios, fields with neutral defaults and the id renames above (the renamed `NT-IOC2` entry, AU-A-34, is in this run).

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

## Earlier results still in force

* **`pair`**: run of 2026-10-06, release executables, `--jobs 2`: **82/82 confirmed, 2 held** (T5, N2); superseded by the
  full run of 2026-11-24, whose detail table is below. Finding NP64 of that run (KobPair had no `scale > 0` check: a maker's own
  ask with a negative scale of A filled for one base unit of B; self-harm only, the builders refuse such terms) was fixed in
  `c0b7a3e` (`require(sScale > 0); require(tScale > 0)`), mutations C9s / C9t confirmed.
* **Engine-level, no mutation.** The overflow scenarios (`V3A05`, `V3B07`, `NV3CB30`, `NV3IA30`, ...: the largest fitting full
  fill validates, one more base unit fails even when paid the exact value) are refused by the engine's checked arithmetic
  (`NumberTooBig`), not by a contract check, so they have no catalog entry.
* **KRON `inputOnly`.** The booking bound and the exit-side 0x08 attack need a custody above 2^53 / 2^55 base units, which a
  KRON token program never accepts (it refuses token outputs above 10^9 base units), so those flips are `inputOnly` there (the
  attacked input's own check is still the only one refusing it).

The records of the earlier runs (the protocol v3 KAS kinds of 2026-10-05/06, the pair phase and the refund rule of
2026-10-06) are in the git history of this file.

## kcc20-sell

KCC-20 v2; test binary `kob_v2_tests`; contracts `contracts/orders`. 339/339 confirmed, 10/10 defence-in-depth held (expected NOT to flip): the full run had 336/338 and 9/9; AU-B-4 and DM-9 were repaired and re-run, DM-9x added.
Baseline (no mutation): v3_merge_push clean, v3_rounding clean, v3_audit_cond_ask_strays clean, v2_repeat_ifd_attacks clean, v2_fix_pass_regressions clean, v2_lifecycle_stop_auction_keeper_trailing clean, v2_touch_cond_settle clean, v2_touch_cond_settle_3x3 clean, audit_pair_sec_tests::aud_ext_kas_stops_refuse_evidence_of_another_extension_commitment clean, v2_negative_trigger_manipulation clean, v2_touch_cond_settle_pm clean, v2_touch_cond_arm clean, v2_touch_cond_trail clean, v2_touch_cond_arm_3x3 clean, v2_touch_cond_trail_3x3 clean, v2_touch_cond_arm_pm clean, v2_touch_cond_trail_pm clean, v2_touch_ifd_fill clean, v2_touch_ifd_arm clean, v2_touch_ifd_arm_3x3 clean, v2_touch_ifd_fill_3x3 clean, v2_touch_ifd_arm_pm clean, v2_touch_ifd_fill_pm clean, v3_min_fill clean, v3_tip clean, v2_lifecycle_stray_custody clean, v3_fok clean, v2_lifecycle_ifd_stop_entry clean, v3_audit_ask clean, v2_lifecycle_auctions_and_kill clean, v2_negative_tif_cancel_expiry clean, v2_negative_limits_and_tips clean, v2_negative_time_variants clean, kob_bid_modes_tests::modes_stray_guard_slots_8x8 clean, v3_audit_bid clean, v3_bid_refund clean, kob_bid_modes_tests::modes_refund_cancel_claim clean, v3_audit_cond_ask clean, kob_bid_modes_tests::modes_fill_negatives clean, v2_negative_oco_and_ifd clean, kob_bid_modes_tests::modes_stray_guard clean, kob_bid_modes_tests::modes_ordering_and_slots clean, v3_audit_ifd_bid clean, kob_v2_buy_tests::v2_buy_touch_cond_arm clean.

| id | contract | check ablated | expected | flipped | status |
|---|---|---|---|---|---|
| A1 | KobCondAsk | TP without the entry only from rptUntil | NRP10 | NRP10 | CONFIRMED |
| A2 | KobCondAsk | stop leg refuses the entry (present == 0) | NRP13 | NRP13 | CONFIRMED |
| A3 | KobCondAsk | entry's merge argument = -(own index * 2^53 + n) (NRP28: the entry's arming update starts with a minimal index push and is also refused by the 0x08 first-byte check, see V-CA-08x; V3CA07: an 8-byte negative ev in the entry's update is refused by the entry itself) | NRP14; stay rejected: NRP28, V3CA07 | NRP14 NRP19(F) | CONFIRMED |
| A4 | KobCondAsk | maker payout floor (partial re-arming TP: maker >= proceeds - budget) | NRP11 | NRP11 | CONFIRMED |
| A5 | KobCondAsk | sell-out: entry gets the budget and the exit carriers | NRP12, V3CA04 | NRP12 V3CA04 | CONFIRMED |
| AU-CA-44 | KobCondAsk | noStrays bound: at most MAX_TOK_IN = 8 token inputs (a 9th is not scanned; the KCC-20 program refuses a 9th input on its own, so only the order flips) | NAU-CA39 | NAU-CA39(F) | CONFIRMED |
| AU-CA-S0 | KobCondAsk | noStrays scan slot 0: a stray of the conditional at token slot 0 rides to the taker | NAU-CAS0 | NAU-CAS0 | CONFIRMED |
| AU-CA-S1 | KobCondAsk | noStrays scan slot 1: a stray of the conditional at token slot 1 rides to the taker | NAU-CAS1 | NAU-CAS1 | CONFIRMED |
| AU-CA-S2 | KobCondAsk | noStrays scan slot 2: a stray of the conditional at token slot 2 rides to the taker | NAU-CAS2 | NAU-CAS2 | CONFIRMED |
| AU-CA-S3 | KobCondAsk | noStrays scan slot 3: a stray of the conditional at token slot 3 rides to the taker | NAU-CAS3 | NAU-CAS3 | CONFIRMED |
| AU-CA-S4 | KobCondAsk | noStrays scan slot 4: a stray of the conditional at token slot 4 rides to the taker | NAU-CAS4 | NAU-CAS4 | CONFIRMED |
| AU-CA-S5 | KobCondAsk | noStrays scan slot 5: a stray of the conditional at token slot 5 rides to the taker | NAU-CAS5 | NAU-CAS5 | CONFIRMED |
| AU-CA-S6 | KobCondAsk | noStrays scan slot 6: a stray of the conditional at token slot 6 rides to the taker | NAU-CAS6 | NAU-CAS6 | CONFIRMED |
| AU-CA-S7 | KobCondAsk | noStrays scan slot 7: a stray of the conditional at token slot 7 rides to the taker | NAU-CAS7 | NAU-CAS7 | CONFIRMED |
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
| TA-cltv | KobCondAsk | touchAsk (settle, stop leg): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTA01, NTA02, NTA03, NTA04 | NTA01 NTA02 NTA03 NTA04 NTA04n4(F) | CONFIRMED |
| TA-interval | KobCondAsk | touchAsk (settle, stop leg): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTA04 | NTA04 | CONFIRMED |
| TA-ivpos | KobCondAsk | touchAsk (settle, stop leg): the evidence interval is not negative (a negative one cancels the UTXO age, so a quote of the same transaction arms the stop; audit 2026-10-29 F1) | NTA04n2, NTA04n3 | NTA04n1(F) NTA04n2 NTA04n3 | CONFIRMED |
| TA-active | KobCondAsk | touchAsk (settle, stop leg): exposure counts the evidence activeFrom | NTA03 | NTA03 | CONFIRMED |
| TA-tokdaa | KobCondAsk | touchAsk (settle, stop leg): exposure counts the evidence ask custody DAA | NTA02 | NTA02 | CONFIRMED |
| TA-tkcov | KobCondAsk | touchAsk (settle, stop leg): the custody input tk carries the token covenant id | NTA02b | NTA02b | CONFIRMED |
| TA-owner | KobCondAsk | touchAsk (settle, stop leg): the custody input tk is owned by the evidence ask (owner = its covenant id, scheme covenant id) | NTA15, NTA16 | NTA15 NTA16 | CONFIRMED |
| TA-tktpl | KobCondAsk | touchAsk (settle, stop leg): the custody input tk is a holder UTXO of the token's program (template hash; P2SH kept) | NTA02c | NTA02c(F) | CONFIRMED |
| TA-tkp2sh | KobCondAsk | touchAsk (settle, stop leg): the custody input tk is the P2SH of the holder redeem script read (template hash kept) | NTA02d | NTA02d(F) | CONFIRMED |
| TA-ext | KobCondAsk | touchAsk (settle, stop leg): the evidence ask's custody tk is of the order's extension commitment (units of the covenant id of another commitment are another token) | NAUD-EXT-CA3 | NAUD-EXT-CA3(F) | CONFIRMED |
| TA-rule | KobCondAsk | settle, unarmed stop leg: the evidence ask quotes <= stopPrice | NTA19, NT3 | NTA19 NT3 | CONFIRMED |
| TU-push08 | KobCondAsk | touch (update: arm on an ask, trail on a bid): the evidence sigscript starts with the fixed 8-byte push of its fill argument (0x08) | NTU10, NTT10, NTT11 | NTU10 NTT10 NTT11 | CONFIRMED |
| TU-npos | KobCondAsk | touch (update: arm on an ask, trail on a bid): the evidence fill argument n > 0 | NTU11 | NTU11 | CONFIRMED |
| TU-tpl | KobCondAsk | touch (update: arm on an ask, trail on a bid): the evidence is an instance of the KobAsk / KobBid template (template hash; P2SH kept) | NTU17, NTT17 | NTU17 NTT17 | CONFIRMED |
| TU-p2sh | KobCondAsk | touch (update: arm on an ask, trail on a bid): the evidence UTXO is the P2SH of the redeem script read (template hash kept) | NTU18, NTT18 | NTU18 NTT18 | CONFIRMED |
| TU-token | KobCondAsk | touch (update: arm on an ask, trail on a bid): the evidence trades this token (tokenCovId) | NTU07b, NTT07 | NTU07b NTT07 | CONFIRMED |
| TU-scale | KobCondAsk | touch (update: arm on an ask, trail on a bid): the evidence quotes per the same scale (prices comparable) | NTU08, NTT08 | NTU08 NTT08 | CONFIRMED |
| TU-mintouch | KobCondAsk | touch (update: arm on an ask, trail on a bid): the evidence fill is >= minTouch base units | NTU09, NTT09 | NTU09 NTT09 | CONFIRMED |
| TU-slope | KobCondAsk | touch (update: arm on an ask, trail on a bid): the evidence does not decay (slope 0) | NTU05, NTT05 | NTU05 NTT05 | CONFIRMED |
| TU-cltv | KobCondAsk | touch (update: arm on an ask, trail on a bid): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTU01, NTU02, NTU03, NTU04, NTT01, NTT03, NTT04 | NTU01 NTU02 NTU03 NTU04 NTU04n4(F) NTT01 NTT03 NTT04 NTT04n4(F) | CONFIRMED |
| TU-interval | KobCondAsk | touch (update: arm on an ask, trail on a bid): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTU04, NTT04 | NTU04 NTT04 | CONFIRMED |
| TU-ivpos | KobCondAsk | touch (update: arm on an ask, trail on a bid): the evidence interval is not negative (a negative one cancels the UTXO age, so a quote of the same transaction arms the stop; audit 2026-10-29 F1) | NTU04n2, NTU04n3, NTT04n2, NTT04n3 | NTU04n1(F) NTU04n2 NTU04n3 NTT04n1(F) NTT04n2 NTT04n3 | CONFIRMED |
| TU-active | KobCondAsk | touch (update: arm on an ask, trail on a bid): exposure counts the evidence activeFrom | NTU03, NTT03 | NTU03 NTT03 | CONFIRMED |
| TU-tokdaa | KobCondAsk | touch (update: arm on an ask, trail on a bid): exposure counts the evidence ask custody DAA | NTU02 | NTU02 | CONFIRMED |
| TU-tkcov | KobCondAsk | touch (update: arm on an ask, trail on a bid): the custody input tk carries the token covenant id | NTU02b | NTU02b | CONFIRMED |
| TU-owner | KobCondAsk | touch (update: arm on an ask, trail on a bid): the custody input tk is owned by the evidence ask (owner = its covenant id, scheme covenant id) | NTU15, NTU16 | NTU15 NTU16 | CONFIRMED |
| TU-tktpl | KobCondAsk | touch (update: arm on an ask, trail on a bid): the custody input tk is a holder UTXO of the token's program (template hash; P2SH kept) | NTU02c | NTU02c(F) | CONFIRMED |
| TU-tkp2sh | KobCondAsk | touch (update: arm on an ask, trail on a bid): the custody input tk is the P2SH of the holder redeem script read (template hash kept) | NTU02d | NTU02d(F) | CONFIRMED |
| TU-ext | KobCondAsk | touch (update: arm on an ask, trail on a bid): the evidence ask's custody tk is of the order's extension commitment (units of the covenant id of another commitment are another token) | NAUD-EXT-CA1 | NAUD-EXT-CA1 | CONFIRMED |
| TU-bidext | KobCondAsk | touch (update: arm on an ask, trail on a bid): the evidence bid names the order's extension commitment (KobBid extensionCommitment [118..150)) | NAUD-EXT-CA2 | NAUD-EXT-CA2 | CONFIRMED |
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
| TI-cltv | KobIfdBid | touchBid (buy-stop entry): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTI01, NTI03, NTI04, NTJ01, NTJ03, NTJ04 | NTI01 NTI03 NTI04 NTI04n4(F) NTJ01 NTJ03 NTJ04 NTJ04n4(F) | CONFIRMED |
| TI-interval | KobIfdBid | touchBid (buy-stop entry): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTI04, NTJ04 | NTI04 NTJ04 | CONFIRMED |
| TI-ivpos | KobIfdBid | touchBid (buy-stop entry): the evidence interval is not negative (a negative one cancels the UTXO age, so a quote of the same transaction arms the stop; audit 2026-10-29 F1) | NTI04n2, NTI04n3, NTJ04n2, NTJ04n3 | NTI04n1(F) NTI04n2 NTI04n3 NTJ04n1(F) NTJ04n2 NTJ04n3 | CONFIRMED |
| TI-active | KobIfdBid | touchBid (buy-stop entry): exposure counts the evidence activeFrom | NTI03, NTJ03 | NTI03 NTJ03 | CONFIRMED |
| TI-bidext | KobIfdBid | touchBid (buy-stop entry): the evidence bid names the order's extension commitment (KobBid extensionCommitment [118..150)) | NAUD-EXT-IB1, NAUD-EXT-IB2 | NAUD-EXT-IB1 NAUD-EXT-IB2(F) | CONFIRMED |
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
| V-B-minfill | KobBid | minFill: n >= minFill unless the fill takes all the buying power left (less than one base unit funded after it) | V3B01, V3B08, V3B08b, V3B09 | V3B08 V3B08b V3B09 V3B01 | CONFIRMED |
| V-B-minfill-end | KobBid | the pre-fix rule (exempt whenever the bid ends, less than one minimum fill left) lets a small fill end a bid funded for under two minimum fills (KobBid 242aaae3 on testnet-10) | V3B08, V3B08b, V3B09 | V3B08 V3B08b V3B09 | CONFIRMED |
| V-B-minfill-one | KobBid | the exemption is strict: a fill that leaves exactly one base unit of buying power is not exempt (< -> <=) | V3B09 | V3B09 | CONFIRMED |
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
| AU-A-1 | KobAsk | tplRs: the custody input is the P2SH of the holder script it pushes (a planted UTXO of the token covenant id) | NAU-A03 | NAU-A03 | CONFIRMED |
| AU-A-2 | KobAsk | tplRs: the custody's template is the token's (a planted P2SH of a look-alike template) | NAU-A04 | NAU-A04 | CONFIRMED |
| AU-A-3 | KobAsk | noStrays bound: at most MAX_TOK_IN = 8 token inputs (a 9th is not scanned; the KCC-20 program refuses a 9th input on its own, so only the ask flips) | NAU-A22 | NAU-A22(F) | CONFIRMED |
| AU-A-S0 | KobAsk | noStrays scan slot 0: a stray of the ask at token slot 0 rides to the taker | NAU-AS0 | NAU-AS0 | CONFIRMED |
| AU-A-S1 | KobAsk | noStrays scan slot 1: a stray of the ask at token slot 1 rides to the taker | NAU-AS1 | NAU-AS1 | CONFIRMED |
| AU-A-S2 | KobAsk | noStrays scan slot 2: a stray of the ask at token slot 2 rides to the taker | NAU-AS2 | NAU-AS2 | CONFIRMED |
| AU-A-S3 | KobAsk | noStrays scan slot 3: a stray of the ask at token slot 3 rides to the taker | NAU-AS3 | NAU-AS3 | CONFIRMED |
| AU-A-S4 | KobAsk | noStrays scan slot 4: a stray of the ask at token slot 4 rides to the taker | NAU-AS4 | NAU-AS4 | CONFIRMED |
| AU-A-S5 | KobAsk | noStrays scan slot 5: a stray of the ask at token slot 5 rides to the taker | NAU-AS5 | NAU-AS5 | CONFIRMED |
| AU-A-S6 | KobAsk | noStrays scan slot 6: a stray of the ask at token slot 6 rides to the taker | NAU-AS6 | NAU-AS6 | CONFIRMED |
| AU-A-S7 | KobAsk | noStrays scan slot 7: a stray of the ask at token slot 7 rides to the taker | NAU-AS7 | NAU-AS7 | CONFIRMED |
| AU-A-4 | KobAsk | the ask is the only input of its covenant id (a sibling UTXO of the id beside the fill) | NAU-A01 | NAU-A01 | CONFIRMED |
| AU-A-5 | KobAsk | the custody carries the token covenant id (not a UTXO of another token of the same program owned by the ask) | NAU-A02 | NAU-A02 | CONFIRMED |
| AU-A-6 | KobAsk | the custody is owned by this ask's covenant id (not tokens of another id: the ask's real custody would be orphaned) | NAU-A05 | NAU-A05 | CONFIRMED |
| AU-A-7 | KobAsk | the custody has owner scheme covenant id and borrowing disabled (0x040100) | NAU-A06 | NAU-A06 | CONFIRMED |
| AU-A-8 | KobAsk | the custody carries the order's extension commitment (another commitment is another token) | NAU-A07 | NAU-A07 | CONFIRMED |
| AU-A-9 | KobAsk | refund only once due: min(expiryDaa, 90 days idle, IOC / FOK kill) | NC8, NC9, NC8b, NC3, NC4 | NC8 NC9 NC8b NC3 NC4 | CONFIRMED |
| AU-A-10 | KobAsk | a refund leaves no output bound to the ask's covenant id (a keeper's UTXO of the id would own every later stray) | NAU-A08 | NAU-A08 | CONFIRMED |
| AU-A-11 | KobAsk | refund: the maker gets the carriers minus at most refundTip | NC6 | NC6 | CONFIRMED |
| AU-A-12 | KobAsk | no fill before activeFrom | NT-ACT | NT-ACT | CONFIRMED |
| AU-A-13 | KobAsk | fill n > 0 (NAU-A10, n = -1, is also refused by n >= minFill \|\| n == amountLeft; AU-A-13x removes both) | stay rejected: NAU-A10 |  | held (defence in depth) |
| AU-A-13x | KobAsk | n > 0 and the minFill rule removed together: a fill of n = -1 (the taker adds a unit, the maker is paid nothing) | NAU-A10 | NAU-A10 | CONFIRMED |
| AU-A-14 | KobAsk | scale > 0 (hostile field: an ask at a negative scale sells for nothing) | NAU-A11 | NAU-A11 | CONFIRMED |
| AU-A-15 | KobAsk | tip >= 0 (hostile field) | NF7 | NF7 | CONFIRMED |
| AU-A-16 | KobAsk | n <= amountLeft: a fill cannot claim more than the ask holds (an evidence fill of 1 base unit claiming minTouch would arm a stop) | NAU-A09 | NAU-A09 | CONFIRMED |
| AU-A-17 | KobAsk | TWAP: the UTXO is >= interval DAA old (CSV) | NV1 | NV1 | CONFIRMED |
| AU-A-18 | KobAsk | TWAP: n <= maxFill | NV2 | NV2 | CONFIRMED |
| AU-A-19 | KobAsk | decay: slope > 0 (a rising ask would reward understated time) | NV7 | NV7 | CONFIRMED |
| AU-A-20 | KobAsk | decay: decayStep > 0 (a negative step makes the price rise: would reward understated time) | NAU-A12 | NAU-A12 | CONFIRMED |
| AU-A-21 | KobAsk | decay: tx.daa >= t (CLTV proves the decay time) | NM3, NV5 | NM3 NV5 | CONFIRMED |
| AU-A-22 | KobAsk | decay: t >= origin (a time before the origin is refused; it could only keep or raise the price) | NAU-A13 | NAU-A13 | CONFIRMED |
| AU-A-23 | KobAsk | positional payout: output i pays P2PK(maker) | NF9 | NF9 | CONFIRMED |
| AU-A-24 | KobAsk | GTC rest: exactly one output bound to the ask's id (no second UTXO of the id for the taker) | NAU-A14 | NAU-A14 | CONFIRMED |
| AU-A-25 | KobAsk | GTC rest: the continuation is this script with amountLeft - n (not a repriced ask) | NAU-A15 | NAU-A15 | CONFIRMED |
| AU-A-26 | KobAsk | GTC rest: the continuation keeps the order's carrier | NF10 | NF10 | CONFIRMED |
| AU-A-27 | KobAsk | GTC rest: the rest custody keeps its carrier | NAU-A16 | NAU-A16 | CONFIRMED |
| AU-A-28 | KobAsk | partial fill: the maker gets at least the all-in proceeds | NF1, NF2, NV6, V3A02 | NF1 NF2 NV6 V3A02 | CONFIRMED |
| AU-A-29 | KobAsk | FOK: a fill that leaves tokens unsold is refused | NAU-A17 | NAU-A17 | CONFIRMED |
| AU-A-30 | KobAsk | termination (IOC, sold out): no output bound to the ask's id | NAU-A18 | NAU-A18 | CONFIRMED |
| AU-A-31 | KobAsk | IOC return: the returned rest keeps the custody's carrier | NAU-A19 | NAU-A19 | CONFIRMED |
| AU-A-32 | KobAsk | IOC: the maker gets the all-in proceeds plus the order carrier | NM1 | NM1 | CONFIRMED |
| AU-A-33 | KobAsk | sold out: the maker gets the all-in proceeds plus both carriers | NAU-A20 | NAU-A20 | CONFIRMED |
| AU-A-34 | KobAsk | the one token output (rest, IOC return, refund) is pinned: the custody template with the expected state | NT-IOC2 | NT-IOC2 | CONFIRMED |
| AU-A-35 | KobAsk | the pinned token output is bound to the token covenant (an unbound copy of the holder script would let the taker take the rest) | NAU-A21 | NAU-A21 | CONFIRMED |
| AU-A-36 | KobAsk | cancel: SIGHASH_ALL only | NC1 | NC1 | CONFIRMED |
| AU-A-37 | KobAsk | cancel: the maker's signature | NC2 | NC2 | CONFIRMED |
| AU-B-1 | KobBid | noStrays bound: at most MAX_TOK_IN = 8 token inputs (a 9th is not scanned; the KCC-20 program refuses a 9th input on its own, so only the bid flips) | NAU-B26 | NAU-B26(F) | CONFIRMED |
| AU-B-S0 | KobBid | noStrays scan slot 0: a stray of the bid at token slot 0 rides to the taker | NAU-BS0, NSS0 | NAU-BS0 NSS0 | CONFIRMED |
| AU-B-S1 | KobBid | noStrays scan slot 1: a stray of the bid at token slot 1 rides to the taker | NAU-BS1, NSS1 | NAU-BS1 NSS1 | CONFIRMED |
| AU-B-S2 | KobBid | noStrays scan slot 2: a stray of the bid at token slot 2 rides to the taker | NAU-BS2, NSS2 | NAU-BS2 NSS2 | CONFIRMED |
| AU-B-S3 | KobBid | noStrays scan slot 3: a stray of the bid at token slot 3 rides to the taker | NAU-BS3, NSS3 | NAU-BS3 NSS3 | CONFIRMED |
| AU-B-S4 | KobBid | noStrays scan slot 4: a stray of the bid at token slot 4 rides to the taker | NAU-BS4, NSS4 | NAU-BS4 NSS4 | CONFIRMED |
| AU-B-S5 | KobBid | noStrays scan slot 5: a stray of the bid at token slot 5 rides to the taker | NAU-BS5, NSS5 | NAU-BS5 NSS5 | CONFIRMED |
| AU-B-S6 | KobBid | noStrays scan slot 6: a stray of the bid at token slot 6 rides to the taker | NAU-BS6, NSS6 | NAU-BS6 NSS6 | CONFIRMED |
| AU-B-S7 | KobBid | noStrays scan slot 7: a stray of the bid at token slot 7 rides to the taker | NAU-BS7, NSS7 | NAU-BS7 NSS7 | CONFIRMED |
| AU-B-2 | KobBid | the bid is the only input of its covenant id (a sibling UTXO of the id beside the fill) | NAU-B01 | NAU-B01 | CONFIRMED |
| AU-B-3 | KobBid | no fill before activeFrom | NAU-B02 | NAU-B02 | CONFIRMED |
| AU-B-4 | KobBid | fill n > 0 (a zero fill would close an exhausted bid before its expiry) | NAU-B03 | NAU-B03 | CONFIRMED (re-run after the scenario fix, see the 2026-11-24 section) |
| AU-B-5 | KobBid | scale > 0 (hostile field: a negative scale turns the quote negative) | NAU-B04 | NAU-B04 | CONFIRMED |
| AU-B-6 | KobBid | tip >= 0 (hostile field: the bid would pay below its quoted price) | NAU-B05 | NAU-B05 | CONFIRMED |
| AU-B-7 | KobBid | reserve >= 0 (hostile field; defence in depth since the delivery modes: the terminal fill's rem + onDelivery >= 0 also refuses the underfunded IOC fill of NAU-B06; AU-B-7x removes both) | stay rejected: NAU-B06 |  | held (defence in depth) |
| AU-B-7x | KobBid | reserve >= 0 and rem + onDelivery >= 0 removed together: a negative reserve lets an IOC bid funded short of the fill's budget fill | NAU-B06 | NAU-B06 | CONFIRMED |
| AU-B-8 | KobBid | deliveryCarrier >= 0 (hostile field) | NAU-B07 | NAU-B07 | CONFIRMED |
| AU-B-9 | KobBid | DCA: the UTXO is >= interval DAA old (CSV) | NV3 | NV3 | CONFIRMED |
| AU-B-10 | KobBid | DCA: n <= maxFill | NV4 | NV4 | CONFIRMED |
| AU-B-11 | KobBid | rising: slope > 0 (a falling bid would reward understated time) | NAU-B08 | NAU-B08 | CONFIRMED |
| AU-B-12 | KobBid | rising: decayStep > 0 (a negative step makes the price fall: would reward understated time) | NAU-B09 | NAU-B09 | CONFIRMED |
| AU-B-13 | KobBid | rising: tx.daa >= t (CLTV proves the time) | NV9 | NV9 | CONFIRMED |
| AU-B-14 | KobBid | rising: t >= origin (a time before the origin is refused; it could only keep or lower the price) | NAU-B10 | NAU-B10 | CONFIRMED |
| AU-B-15 | KobBid | a fill never spends the reserve: left >= reserve | NAU-B11 | NAU-B11 | CONFIRMED |
| AU-B-16 | KobBid | the delivery template is read from a token input of this token (the bytes are also hash-pinned, so a same-program source of another covenant gains nothing; the check pins the source) | NAU-B12 | NAU-B12 | CONFIRMED |
| AU-B-17 | KobBid | the delivery template's hash is the token's (a planted look-alike would give the maker an anyone-can-spend fake) | NAU-B13 | NAU-B13 | CONFIRMED |
| AU-B-18 | KobBid | positional delivery: output i is the holder of exactly n base units owned by the maker | NAU-B14, NM01, NC01, NC09, NT01, NF05 | NAU-B14 NM01 NM02 NM02b NM04 NM06 NF03 NF04 NF05 NF06 NC01 NC03 NC09 NC10 NC11 NC13 NC14(F) NT01 NT09(F) | CONFIRMED |
| AU-B-19 | KobBid | the delivery is bound to the token covenant (an unbound copy of the holder script delivers nothing) | NAU-B15 | NAU-B15 | CONFIRMED |
| AU-B-20 | KobBid | GTC continuation: exactly one output bound to the bid's id | NAU-B16 | NAU-B16 | CONFIRMED |
| AU-B-21 | KobBid | GTC continuation: this script (not a bid of the taker) | NAU-B17, NC06 | NAU-B17 NC06 | CONFIRMED |
| AU-B-22 | KobBid | GTC continuation: exactly the unused escrow | NF5, NF6, NM07, NM07b, NC08, NT11 | NF5 NF6 NM07 NM07b NC08 NT11 | CONFIRMED |
| AU-B-23 | KobBid | GTC: the delivery carries the carrier plus the budget not spent | NF4, V3B03, NC07 | NF4 V3B03 NC07 NT06(F) | CONFIRMED |
| AU-B-24 | KobBid | termination: no output bound to the bid's id | NAU-B18 | NAU-B18 | CONFIRMED |
| AU-B-25 | KobBid | termination below REFUND_MIN: everything not spent rides on the delivery | NF3, NV8, BR28, NC12 | NF3 NV8 BR28 NC12 | CONFIRMED |
| AU-B-26 | KobBid | cancel: SIGHASH_ALL only | NAU-B24 | NAU-B24 | CONFIRMED |
| AU-B-27 | KobBid | cancel: the maker's signature | NAU-B25 | NAU-B25 | CONFIRMED |
| AU-B-28 | KobBid | refund only once due: min(expiryDaa, 90 days idle, IOC / FOK kill) | NC10, NC10b, NC3b, NC5 | NC10 NC10b NC3b NC5 | CONFIRMED |
| AU-B-29 | KobBid | refund: the bid is the only input of its covenant id | NAU-B19 | NAU-B19 | CONFIRMED |
| AU-B-30 | KobBid | refund: no token input (strays owned by the id stay put) | NS6, NR11 | NS6 NR11 | CONFIRMED |
| AU-B-31 | KobBid | refund: no output bound to the bid's id | NAU-B21 | NAU-B21 | CONFIRMED |
| AU-B-32 | KobBid | refund: output i pays P2PK(maker) | NC7, NAU-B22 | NC7 NAU-B22 | CONFIRMED |
| AU-B-33 | KobBid | refund: the maker gets the escrow minus at most refundTip | NAU-B23 | NAU-B23 | CONFIRMED |
| AU-CA-1 | KobCondAsk | tplRs: the custody input is the P2SH of the holder script it pushes (a planted UTXO of the token covenant id) | NAU-CA03 | NAU-CA03 | CONFIRMED |
| AU-CA-2 | KobCondAsk | tplRs: the custody's template is the token's (a planted P2SH of a look-alike template) | NAU-CA04 | NAU-CA04 | CONFIRMED |
| AU-CA-3 | KobCondAsk | tplState (evidence and its custody): the input is the P2SH of the redeem script read (a non-P2SH evidence input) | NTU18, NTA18, NTT18 | NTU02d(F) NTU18 NTA02d(F) NTA18 NTT18 | CONFIRMED |
| AU-CA-4 | KobCondAsk | tplState (evidence and its custody): the template hash (a look-alike evidence order) | NT6, NTU17, NTA17, NTT17 | NT6 NTU02c(F) NTU17 NTA02c(F) NTA17 NTT17 | CONFIRMED |
| AU-CA-5 | KobCondAsk | settle: the order is the only input of its covenant id (a sibling UTXO of the id) | NAU-CA01 | NAU-CA01 | CONFIRMED |
| AU-CA-6 | KobCondAsk | the custody carries the token covenant id (not another token of the same program owned by the order) | NAU-CA02 | NAU-CA02 | CONFIRMED |
| AU-CA-7 | KobCondAsk | the custody is owned by this order's covenant id | NAU-CA05 | NAU-CA05 | CONFIRMED |
| AU-CA-8 | KobCondAsk | the custody has owner scheme covenant id and borrowing disabled (0x040100) | NAU-CA06 | NAU-CA06 | CONFIRMED |
| AU-CA-9 | KobCondAsk | the custody carries the order's extension commitment | NAU-CA07 | NAU-CA07 | CONFIRMED |
| AU-CA-10 | KobCondAsk | the custody holds exactly amountLeft (a dust UTXO of the id cannot stand in for it) | NS7b | NS7b | CONFIRMED |
| AU-CA-11 | KobCondAsk | refund only from min(expiryDaa, 90 days idle) | NAU-CA08 | NAU-CA08 | CONFIRMED |
| AU-CA-12 | KobCondAsk | a refund leaves no output bound to the order's id | NAU-CA09 | NAU-CA09 | CONFIRMED |
| AU-CA-13 | KobCondAsk | refund: the maker gets the carriers minus at most refundTip | NAU-CA10 | NAU-CA10 | CONFIRMED |
| AU-CA-14 | KobCondAsk | no fill before activeFrom | NAU-CA11 | NAU-CA11 | CONFIRMED |
| AU-CA-15 | KobCondAsk | fill n > 0 (NAU-CA19, n = -1, is also refused by n >= minFill \|\| n == amountLeft; AU-CA-15x removes both) | stay rejected: NAU-CA19 |  | held (defence in depth) |
| AU-CA-15x | KobCondAsk | n > 0 and the minFill rule removed together: a fill of n = -1 (the taker adds a unit, the maker is paid nothing) | NAU-CA19 | NAU-CA19 | CONFIRMED |
| AU-CA-16 | KobCondAsk | scale > 0 (hostile field: a negative scale sells for nothing) | NAU-CA12 | NAU-CA12 | CONFIRMED |
| AU-CA-17 | KobCondAsk | tip >= 0 (hostile field) | NAU-CA13 | NAU-CA13 | CONFIRMED |
| AU-CA-18 | KobCondAsk | n <= amountLeft (a fill claiming more than the custody holds) | NAU-CA22 | NAU-CA22 | CONFIRMED |
| AU-CA-19 | KobCondAsk | the TP leg needs tpPrice > 0 (a stop-only order would sell on the TP leg for nothing) | NAU-CA23 | NAU-CA23 | CONFIRMED |
| AU-CA-20 | KobCondAsk | leg is 0 or 1 | NAU-CA24 | NAU-CA24 | CONFIRMED |
| AU-CA-21 | KobCondAsk | the stop leg needs stopPrice > 0 | NAU-CA25 | NAU-CA25 | CONFIRMED |
| AU-CA-22 | KobCondAsk | slipBps >= 0 (hostile field) | NAU-CA26 | NAU-CA26 | CONFIRMED |
| AU-CA-23 | KobCondAsk | slipBps <= 10000 (hostile field: a band wider than the stop price; outside an auction the leg price would also go negative) | NAU-CA27 | NAU-CA27 | CONFIRMED |
| AU-CA-24 | KobCondAsk | stop auction: tx.daa >= t (CLTV proves the auction time) | NA2 | NA2 | CONFIRMED |
| AU-CA-25 | KobCondAsk | stopPrice <= MAX_STOP (the band product must fit; with slipBps 0 an oversized stop would sell) | NAU-CA28 | NAU-CA28 | CONFIRMED |
| AU-CA-26 | KobCondAsk | positional payout: output i pays P2PK(maker) | NAU-CA17 | NAU-CA17 | CONFIRMED |
| AU-CA-27 | KobCondAsk | rest: exactly one output bound to the order's id | NAU-CA14 | NAU-CA14 | CONFIRMED |
| AU-CA-28 | KobCondAsk | rest: the continuation is this script with the spliced windows (not a repriced order) | NAU-CA15 | NAU-CA15 | CONFIRMED |
| AU-CA-29 | KobCondAsk | rest: the continuation keeps the order's carrier | NAU-CA29 | NAU-CA29 | CONFIRMED |
| AU-CA-30 | KobCondAsk | rest: the rest custody keeps its carrier | NAU-CA16 | NAU-CA16 | CONFIRMED |
| AU-CA-31 | KobCondAsk | sold out: no output bound to the order's id | NAU-CA18 | NAU-CA18 | CONFIRMED |
| AU-CA-32 | KobCondAsk | re-arming sell-out: the maker gets the profit (all-in minus the budget) | NAU-CA31 | NAU-CA31 | CONFIRMED |
| AU-CA-33 | KobCondAsk | sold out: the maker gets the all-in proceeds plus both carriers | NAU-CA20 | NAU-CA20 | CONFIRMED |
| AU-CA-34 | KobCondAsk | the one token output (rest, refund) is pinned (template, amount, owner) | NAU-CA30 | NAU-CA30 | CONFIRMED |
| AU-CA-35 | KobCondAsk | the pinned token output is bound to the token covenant | NAU-CA21 | NAU-CA21 | CONFIRMED |
| AU-CA-36 | KobCondAsk | update: the order is the only input of its covenant id | NAU-CA32 | NAU-CA32 | CONFIRMED |
| AU-CA-37 | KobCondAsk | update: stopPrice > 0 (an order without a stop leg is never armed or trailed) | NAU-CA33 | NAU-CA33 | CONFIRMED |
| AU-CA-38 | KobCondAsk | update: not before activeFrom | NAU-CA34 | NAU-CA34 | CONFIRMED |
| AU-CA-39 | KobCondAsk | update: exactly one output bound to the order's id | NAU-CA35 | NAU-CA35 | CONFIRMED |
| AU-CA-40 | KobCondAsk | update: the continuation changes only stopPrice / armed as the evidence justifies | NAU-CA36 | NAU-CA36 | CONFIRMED |
| AU-CA-41 | KobCondAsk | update: the keeper takes at most keeperTip | NK1 | NK1 | CONFIRMED |
| AU-CA-42 | KobCondAsk | cancel: SIGHASH_ALL only | NAU-CA37 | NAU-CA37 | CONFIRMED |
| AU-CA-43 | KobCondAsk | cancel: the maker's signature | NAU-CA38 | NAU-CA38 | CONFIRMED |
| AU-IB-1 | KobIfdBid | tplState (evidence bid, merge exit): the input is the P2SH of the redeem script read | NTI18, NTJ18 | NTI18 NTJ18 | CONFIRMED |
| AU-IB-2 | KobIfdBid | tplState (evidence bid, merge exit): the template hash | NTI17, NTJ17 | NTI17 NTJ17 | CONFIRMED |
| AU-IB-3 | KobIfdBid | noStrays bound: at most MAX_TOK_IN = 8 token inputs (a 9th is not scanned; the KCC-20 program refuses a 9th input on its own, so only the entry flips) | NAU-IB30 | NAU-IB30(F) | CONFIRMED |
| AU-IB-S0 | KobIfdBid | noStrays scan slot 0: a stray of the entry at token slot 0 rides to the taker | NAU-IBS0 | NAU-IBS0 | CONFIRMED |
| AU-IB-S1 | KobIfdBid | noStrays scan slot 1: a stray of the entry at token slot 1 rides to the taker | NAU-IBS1 | NAU-IBS1 | CONFIRMED |
| AU-IB-S2 | KobIfdBid | noStrays scan slot 2: a stray of the entry at token slot 2 rides to the taker | NAU-IBS2 | NAU-IBS2 | CONFIRMED |
| AU-IB-S3 | KobIfdBid | noStrays scan slot 3: a stray of the entry at token slot 3 rides to the taker | NAU-IBS3 | NAU-IBS3 | CONFIRMED |
| AU-IB-S4 | KobIfdBid | noStrays scan slot 4: a stray of the entry at token slot 4 rides to the taker | NAU-IBS4 | NAU-IBS4 | CONFIRMED |
| AU-IB-S5 | KobIfdBid | noStrays scan slot 5: a stray of the entry at token slot 5 rides to the taker | NAU-IBS5 | NAU-IBS5 | CONFIRMED |
| AU-IB-S6 | KobIfdBid | noStrays scan slot 6: a stray of the entry at token slot 6 rides to the taker | NAU-IBS6 | NAU-IBS6 | CONFIRMED |
| AU-IB-S7 | KobIfdBid | noStrays scan slot 7: a stray of the entry at token slot 7 rides to the taker | NAU-IBS7 | NAU-IBS7 | CONFIRMED |
| AU-IB-4 | KobIfdBid | fill / merge: the entry is the only input of its covenant id | NAU-IB01 | NAU-IB01 | CONFIRMED |
| AU-IB-5 | KobIfdBid | scale > 0 (hostile field) | NAU-IB02 | NAU-IB02 | CONFIRMED |
| AU-IB-6 | KobIfdBid | tip >= 0 (hostile field) | NAU-IB03 | NAU-IB03 | CONFIRMED |
| AU-IB-7 | KobIfdBid | merge: the exit terms [189..244) (slipBps .. minRestDaa) are the committed exitState | NAU-IB04 | NAU-IB04 | CONFIRMED |
| AU-IB-8 | KobIfdBid | merge: the exit terms [252..271) (bandDaa, keeperTip) are the committed exitState | NAU-IB05 | NAU-IB05 | CONFIRMED |
| AU-IB-9 | KobIfdBid | merge: the exit's extension commitment is the entry's | NAU-IB06 | NAU-IB06 | CONFIRMED |
| AU-IB-10 | KobIfdBid | no fill before activeFrom | NAU-IB07 | NAU-IB07 | CONFIRMED |
| AU-IB-11 | KobIfdBid | fill n > 0 (a zero fill of an empty repeating entry would book an empty exit and move carriers out of the escrow) | NAU-IB08 | NAU-IB08 | CONFIRMED |
| AU-IB-12 | KobIfdBid | n <= amountLeft | NI10 | NI10 | CONFIRMED |
| AU-IB-13 | KobIfdBid | deliveryCarrier >= 0 (hostile field) | NAU-IB09 | NAU-IB09 | CONFIRMED |
| AU-IB-14 | KobIfdBid | exitCarrier >= 0 (hostile field) | NAU-IB10 | NAU-IB10 | CONFIRMED |
| AU-IB-15 | KobIfdBid | stop entry: entryStop <= price | NI19 | NI19 | CONFIRMED |
| AU-IB-16 | KobIfdBid | stop auction: tx.daa >= t (CLTV) | NAU-IB31 | NAU-IB31 | CONFIRMED |
| AU-IB-17 | KobIfdBid | stop auction: t >= origin | NAU-IB32 | NAU-IB32 | CONFIRMED |
| AU-IB-18 | KobIfdBid | repeat: a fill that is not booked leaves fewer re-arms than one minimum fill | NAU-IB33 | NAU-IB33 | CONFIRMED |
| AU-IB-19 | KobIfdBid | the exit is built from the KobCondAsk template (cPre / cSuf hash): a look-alike template around the committed state would make the exit and its tokens anyone-can-spend (NI4, the KobAsk template beside a KobCondAsk output, is also refused by the exit script equality) | NAU-IB34; stay rejected: NI4 | NAU-IB34 | CONFIRMED |
| AU-IB-20 | KobIfdBid | output exitOut is the committed exit (defence in depth: the exit covenant id is derived from exitSpk, so a tampered exit output also fails the genesis id check; AU-IB-20x removes both) | stay rejected: NI2, NI12 |  | held (defence in depth) |
| AU-IB-21 | KobIfdBid | the exit's covenant id is a fresh genesis of this entry's outpoint (NI7, the entry's own id, is also refused by the continuation count) | NI6, NI8; stay rejected: NI7 | NI6 NI8 | CONFIRMED |
| AU-IB-20x | KobIfdBid | the exit script equality and the genesis id check removed together: a tampered exit (stop leg removed) is accepted | NI2 | NI2 NI2b NI6 NI8 | CONFIRMED |
| AU-IB-22 | KobIfdBid | partial fill: the exit carries exitCarrier | NI5c | NI5c | CONFIRMED |
| AU-IB-23 | KobIfdBid | final fill: the exit gets everything left | NI5 | NI5 | CONFIRMED |
| AU-IB-24 | KobIfdBid | the delivery template is read from a token input of this token (bytes also hash-pinned; pins the source) | NAU-IB12 | NAU-IB12 | CONFIRMED |
| AU-IB-25 | KobIfdBid | the delivery template's hash is the token's (a planted look-alike would give the exit an anyone-can-spend fake) | NAU-IB13 | NAU-IB13 | CONFIRMED |
| AU-IB-26 | KobIfdBid | positional delivery: output i holds n base units owned by the exit's covenant id | NI3 | NI1 NI3 | CONFIRMED |
| AU-IB-27 | KobIfdBid | the delivery is bound to the token covenant | NAU-IB14 | NAU-IB14 | CONFIRMED |
| AU-IB-28 | KobIfdBid | the delivery carries deliveryCarrier | NAU-IB15 | NAU-IB15 | CONFIRMED |
| AU-IB-29 | KobIfdBid | continuation: exactly one output bound to the entry's id | NAU-IB35 | NAU-IB35 | CONFIRMED |
| AU-IB-30 | KobIfdBid | continuation: the escrow floor | NI5b | NI5b | CONFIRMED |
| AU-IB-31 | KobIfdBid | final fill: no output bound to the entry's id | NAU-IB16 | NAU-IB16 | CONFIRMED |
| AU-IB-32 | KobIfdBid | update: the entry is the only input of its covenant id | NAU-IB17 | NAU-IB17 | CONFIRMED |
| AU-IB-33 | KobIfdBid | update: only a stop entry is armed | NI20 | NI20 | CONFIRMED |
| AU-IB-34 | KobIfdBid | update: only an unarmed entry is armed (an armed auction would restart) | NAU-IB18 | NAU-IB18 | CONFIRMED |
| AU-IB-35 | KobIfdBid | update: keeperTip >= 0 (hostile field) | NAU-IB19 | NAU-IB19 | CONFIRMED |
| AU-IB-36 | KobIfdBid | update: not before activeFrom | NAU-IB20 | NAU-IB20 | CONFIRMED |
| AU-IB-37 | KobIfdBid | update: exactly one output bound to the entry's id | NAU-IB21 | NAU-IB21 | CONFIRMED |
| AU-IB-38 | KobIfdBid | update: the continuation changes only armed | NI23 | NI23 | CONFIRMED |
| AU-IB-39 | KobIfdBid | update: the keeper takes at most keeperTip | NI21 | NI21 | CONFIRMED |
| AU-IB-40 | KobIfdBid | cancel: SIGHASH_ALL only | NAU-IB22 | NAU-IB22 | CONFIRMED |
| AU-IB-41 | KobIfdBid | cancel: the maker's signature | NAU-IB23 | NAU-IB23 | CONFIRMED |
| AU-IB-42 | KobIfdBid | refund only from min(expiryDaa, 90 days idle) | NAU-IB24 | NAU-IB24 | CONFIRMED |
| AU-IB-43 | KobIfdBid | refund: the entry is the only input of its covenant id | NAU-IB25 | NAU-IB25 | CONFIRMED |
| AU-IB-44 | KobIfdBid | refund: no token input | NAU-IB26 | NAU-IB26 | CONFIRMED |
| AU-IB-45 | KobIfdBid | refund: no output bound to the entry's id | NAU-IB27 | NAU-IB27 | CONFIRMED |
| AU-IB-46 | KobIfdBid | refund: output i pays P2PK(maker) | NAU-IB28 | NAU-IB28 | CONFIRMED |
| AU-IB-47 | KobIfdBid | refund: the maker gets the escrow minus at most refundTip | NAU-IB29 | NAU-IB29 | CONFIRMED |
| BR-B-spk | KobBid | refundTo: the slot pays P2PK(maker), the destination of refund() (a redirected, P2SH or omitted refund; there the slot holds the taker's change) | BR21, BR22, BR23, BR25, BR27, BR29, BR32 | BR21 BR22 BR23 BR25 BR27 BR29 BR32 | CONFIRMED |
| BR-B-value | KobBid | refundTo: the slot holds at least the remainder (one sompi short, an empty or smaller refund at the slot; the market bid charged its bound, spend one sompi above the floor) | BR24, BR33, BR34, NM4, V3B03b | BR24 BR33 BR34 NM4 V3B03b | CONFIRMED |
| BR-B-alias | KobBid | refundTo aliasing guard: the slot is not the positional output of another order / router input (an ask of the same maker paying its proceeds there, any other covenant input) | BR37, BR38 | BR37 BR38 | CONFIRMED |
| BR-B-slot | KobBid | refundTo: one slot per bid input (M - 1 - self): without the - self two bids of one maker share the last output | BR32, BR33 | BR32 BR33 | CONFIRMED [positive scenarios rejected: BR09+] |
| BR-B-carrier | KobBid | terminal fill with a refund: the delivery still carries deliveryCarrier | BR26, NC18 | BR26 NC18 | CONFIRMED |
| BR-B-branch | KobBid | the branch condition flipped (rem < REFUND_MIN): a remainder of REFUND_MIN or more rides on the delivery again, one below it needs the refund layout | BR25, BR27, BR28 | BR25 BR27 BR28 BR36(F) | CONFIRMED [positive scenarios rejected: BR01+ BR02+ BR03+ BR04+ BR05+ BR06+ BR07+ BR08+ BR09+ BR10+ BR11+ BR12+ BR13+] |
| BR-B-above | KobBid | the boundary moved up (rem > REFUND_MIN): a remainder of exactly REFUND_MIN rides on the delivery | BR27 | BR27 | CONFIRMED [positive scenarios rejected: BR02+] |
| BR-B-below | KobBid | the boundary moved down (rem >= REFUND_MIN - 1): a remainder of REFUND_MIN - 1 sompi is split off to the maker and the delivery keeps only the carrier | BR28 | BR28 | CONFIRMED [positive scenarios rejected: BR03+] |
| DM-1 | KobBid | fill: bought >= 0 (hostile field: a mode-1 order with bought -5 delivers a custody of n - 5, the seller keeps 5) | NS05 | NS05 | CONFIRMED |
| DM-2 | KobBid | stray guard: only a mode-1 order holding a custody (bought > 0) excludes tokIn from the scan; otherwise a stray of the id named as tokIn rides to the seller | NG02, NG03, NG04, NG05 | NG02 NG03 NG04 NG05 | CONFIRMED |
| DM-3 | KobBid | mode >= 0 (hostile field: mode -1 would fill as mode 0) | NS02 | NS02 | CONFIRMED |
| DM-4 | KobBid | mode <= 2 (hostile field: mode 3 would fill as mode 0) | NS01 | NS01 | CONFIRMED |
| DM-5 | KobBid | modes 0 and 2 require bought == 0 (a mode-0 / mode-2 order with bought 5 would carry a dead field) | NS03, NS04 | NS03 NS04 | CONFIRMED |
| DM-6 | KobBid | mode 2 forms 1 and 2: recvThreshold > 0 (a receive UTXO of threshold 0 can be churned for free; a top-up into any threshold) | NS06, NS07 | NS06 NS07 | CONFIRMED |
| DM-7 | KobBid | mode 2: topupTip <= deliveryCarrier (the filler of a top-up takes more than the carrier it saves) | NS08 | NS08 | CONFIRMED |
| DM-8 | KobBid | mode 2: form is 0, 1 or 2 (form 3 / -1 would fill as a top-up) | NF01, NF02 | NF01 NF02 | CONFIRMED |
| DM-9 | KobBid | top-up: the receive UTXO is the P2SH of the script its state is read from. Defence in depth since DLV-07: the non-P2SH look-alike of 0 units (NT10, KCC20Ref only) authorises no output, so the successor check OpAuthOutputIdx(tokIn, 0) == self refuses it too; DM-9x removes both | stay rejected: NT10 |  | held (defence in depth) (re-run after the scenario fix, see the 2026-11-24 section) |
| DM-9x | KobBid | top-up: the P2SH check and the successor check removed: a non-P2SH look-alike of 0 units while the real 1,000 lead, the seller keeps 1,000 of the maker's (KCC20Ref only) | NT10 | NT10 | CONFIRMED (added and run after the full run) |
| DM-10 | KobBid | top-up: the receive UTXO's owner is the maker with schemes 0x00 / 0x01 (defence in depth: the delivery's owner is the maker and the token's borrow path keeps the owner, so another key's receive UTXO stays refused) | stay rejected: NT02 |  | held (defence in depth) |
| DM-11 | KobBid | top-up: the receive UTXO's threshold is at least recvThreshold (a threshold-10 UTXO is cheap to churn) | NT03 | NT03 | CONFIRMED |
| DM-12 | KobBid | top-up: the receive UTXO's guard is canonical (24 zero bytes after the threshold) | NT04 | NT04 | CONFIRMED |
| DM-13 | KobBid | top-up: the receive UTXO carries the order's commitment (defence in depth: the delivery is written with extensionCommitment and the token keeps the successor's) | stay rejected: NT05 |  | held (defence in depth) |
| DM-14 | KobBid | top-up: the escrow releases topupTip, not the carrier | NT11 | NT11 | CONFIRMED |
| DM-15 | KobBid | mode 1 with a custody: no carrier leaves the escrow | NC08 | NC08 | CONFIRMED |
| DM-16 | KobBid | mode 1 with a custody: no carrier is reserved per further fill (else a dust fill below minFill ends an order that can still buy one) | NC16 | NC16 | CONFIRMED |
| DM-17 | KobBid | mode 1: the custody holds bought + n (else the seller keeps the bought tokens and the order restarts at n) | NC17 | NC17 | CONFIRMED |
| DM-18 | KobBid | mode 1: the custody's KAS stays on it (continuing and ending) | NC07, NC12 | NC07 NC12 NC18 | CONFIRMED |
| DM-19 | KobBid | mode 1 with a custody: tokIn is the custody (isCustody), not another UTXO of the id | NC02, NC15 | NC02 NC15 | CONFIRMED |
| DM-20 | KobBid | isCustody: the custody is known by its exact state (amount == bought, owner the id, 0x04 / 0x00, zero guard, commitment): a 1-unit or borrow-enabled UTXO of the id, or a 10,000-unit stray in a refund | NC02, NC15, NR12 | NC02 NC15 NR12 | CONFIRMED |
| DM-21 | KobBid | mode 1 continuing: the delivery is the bought-custody (owner the id, scheme 0x04), not a UTXO of the maker | NC03, NM04 | NM04 NC03 | CONFIRMED |
| DM-22 | KobBid | mode 2 form 1: the new receive UTXO's threshold is recvThreshold | NF05 | NF05 | CONFIRMED |
| DM-23 | KobBid | forms 1 and 2 only in mode 2 (a mode-0 order whose state carries mode 2's fields stays mode 0) | NM02b, NM03b | NM02b NM03b | CONFIRMED |
| DM-24 | KobBid | the continuation is the script with bought spliced (not the unchanged script) | NC06 | NC06 | CONFIRMED |
| DM-25 | KobBid | refund with bought > 0 takes the custody branch (else the custody is left behind) | NR01 | NR01 | CONFIRMED [positive scenarios rejected: PR01 PR02] |
| DM-26 | KobBid | refund with a custody: it is the only token input (a stray of the id beside it would ride along) | NR08 | NR08 | CONFIRMED |
| DM-27 | KobBid | refund: custIn is the custody (isCustody) | NR12 | NR12 | CONFIRMED |
| DM-28 | KobBid | refund: the custody goes whole to the maker's key as one plain UTXO at custIn | NR02, NR04, NR05, NR07 | NR02 NR04 NR05 NR07 | CONFIRMED |
| DM-29 | KobBid | refund: the release is bound to the token covenant (an unbound copy of the release script holds nothing) | NR13 | NR13 | CONFIRMED |
| DM-30 | KobBid | refund: the release keeps the custody's KAS | NR03 | NR03 | CONFIRMED |
| DM-31 | KobBid | top-up: output i is the receive UTXO's successor (DLV-07: a second bid claims the receive UTXO another bid tops up, or one bid leaves the successor to another output) | NO07, NO08 | NO01(F) NO03(F) NO04(F) NO07 NO08 NO05(F) | CONFIRMED |
| DM-32 | KobBid | refund takes no argument (H1: refund(int custIn) pushed as 8 bytes reads as a fill of n, so a due refund arms or trails stops) | NTT11c, NTJ11c, NTV11c | NTT11c NTJ11c NTV11c | CONFIRMED |

Collateral flips (scenarios accepted besides the expected ones):

- A3: NRP19
- B5: NRP3 NRP3b NRP16 NRP17b NRP18 NRP29
- FXL3b: NRP20
- TA-cltv: NTA04n4
- TA-ivpos: NTA04n1
- TU-cltv: NTU04n4 NTT04n4
- TU-ivpos: NTU04n1 NTT04n1
- TI-push08: NTI11
- TI-cltv: NTI04n4 NTJ04n4
- TI-ivpos: NTI04n1 NTJ04n1
- V-CA-08x: NRP14 NRP19
- AU-B-18: NM02 NM02b NM04 NM06 NF03 NF04 NF06 NC03 NC10 NC11 NC13 NC14 NT09
- AU-B-23: NT06
- AU-CA-3: NTU02d NTA02d
- AU-CA-4: NTU02c NTA02c
- AU-IB-20x: NI2b NI6 NI8
- AU-IB-26: NI1
- BR-B-branch: BR36
- DM-18: NC18
- DM-31: NO01 NO03 NO04 NO05

## kcc20-buy

KCC-20 v3; test binary `kob_v2_buy_tests`; contracts `contracts/orders`. 255/255 confirmed, 8/8 defence-in-depth held (expected NOT to flip).
Baseline (no mutation): v3_buy_merge_base clean, v2_buy_lifecycle clean, v2_buy_touch_cond_settle clean, v2_buy_repeat_ifd_attacks clean, v2_buy_touch_cond_settle_3x3 clean, audit_pair_sec_tests::aud_ext_kas_stops_refuse_evidence_of_another_extension_commitment clean, v2_buy_touch_cond_settle_pm clean, v2_buy_touch_cond_arm clean, v2_buy_negative clean, v2_buy_touch_cond_trail clean, v2_buy_touch_cond_arm_3x3 clean, v2_buy_touch_cond_trail_3x3 clean, v2_buy_touch_cond_arm_pm clean, v2_buy_touch_cond_trail_pm clean, v2_buy_touch_ifd_arm clean, v2_buy_touch_ifd_fill clean, v2_buy_touch_ifd_arm_3x3 clean, v2_buy_touch_ifd_fill_3x3 clean, v2_buy_touch_ifd_arm_pm clean, v2_buy_touch_ifd_fill_pm clean, v3_buy_min_fill clean, v3_buy_rounding clean, v3_buy_tip clean, v3_buy_merge_push clean, empty_repeating_ifda_refund_is_refused clean, v3_buy_audit_cond_bid clean, v3_buy_cond_refund clean, dm_delivery_rules_negative clean, dm_second_topup clean, dm_stray_guard clean, dm_ifd_exit_fields clean, v3_buy_audit_ifd_ask clean, dm_refund_releases_the_custody clean, dm_trigger_interplay clean.

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
| TB-cltv | KobCondBid | touchBid (settle, buy-stop leg): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTB01, NTB03, NTB04 | NTB01 NTB03 NTB04 NTB04n4(F) | CONFIRMED |
| TB-interval | KobCondBid | touchBid (settle, buy-stop leg): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTB04 | NTB04 | CONFIRMED |
| TB-ivpos | KobCondBid | touchBid (settle, buy-stop leg): the evidence interval is not negative (a negative one cancels the UTXO age, so a quote of the same transaction arms the stop; audit 2026-10-29 F1) | NTB04n2, NTB04n3 | NTB04n1(F) NTB04n2 NTB04n3 | CONFIRMED |
| TB-active | KobCondBid | touchBid (settle, buy-stop leg): exposure counts the evidence activeFrom | NTB03 | NTB03 | CONFIRMED |
| TB-bidext | KobCondBid | touchBid (settle, buy-stop leg): the evidence bid names the order's extension commitment (KobBid extensionCommitment [118..150)) | NAUD-EXT-CB3 | NAUD-EXT-CB3(F) | CONFIRMED |
| TB-rule | KobCondBid | settle, unarmed buy-stop leg: the evidence bid quotes >= stopPrice | NTB19, NB13 | NTB19 NB13 | CONFIRMED |
| TV-push08 | KobCondBid | touch (update: arm on a bid, trail on an ask): the evidence sigscript starts with the fixed 8-byte push of its fill argument (0x08) | NTV10, NTV11, NTW10 | NTV10 NTV11 NTW10 | CONFIRMED |
| TV-npos | KobCondBid | touch (update: arm on a bid, trail on an ask): the evidence fill argument n > 0 | NTW11 | NTW11 | CONFIRMED |
| TV-tpl | KobCondBid | touch (update: arm on a bid, trail on an ask): the evidence is an instance of the KobAsk / KobBid template (template hash; P2SH kept) | NTV17, NTW17 | NTV17 NTW17 | CONFIRMED |
| TV-p2sh | KobCondBid | touch (update: arm on a bid, trail on an ask): the evidence UTXO is the P2SH of the redeem script read (template hash kept) | NTV18, NTW18 | NTV18 NTW18 | CONFIRMED |
| TV-token | KobCondBid | touch (update: arm on a bid, trail on an ask): the evidence trades this token (tokenCovId) | NTV07, NTW07b | NTV07 NTW07b | CONFIRMED |
| TV-scale | KobCondBid | touch (update: arm on a bid, trail on an ask): the evidence quotes per the same scale (prices comparable) | NTV08, NTW08 | NTV08 NTW08 | CONFIRMED |
| TV-mintouch | KobCondBid | touch (update: arm on a bid, trail on an ask): the evidence fill is >= minTouch base units | NTV09, NTW09 | NTV09 NTW09 | CONFIRMED |
| TV-slope | KobCondBid | touch (update: arm on a bid, trail on an ask): the evidence does not decay (slope 0) | NTV05, NTW05 | NTV05 NTW05 | CONFIRMED |
| TV-cltv | KobCondBid | touch (update: arm on a bid, trail on an ask): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTV01, NTV03, NTV04, NTW01, NTW02, NTW03, NTW04 | NTV01 NTV03 NTV04 NTV04n4(F) NTW01 NTW02 NTW03 NTW04 NTW04n4(F) | CONFIRMED |
| TV-interval | KobCondBid | touch (update: arm on a bid, trail on an ask): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTV04, NTW04 | NTV04 NTW04 | CONFIRMED |
| TV-ivpos | KobCondBid | touch (update: arm on a bid, trail on an ask): the evidence interval is not negative (a negative one cancels the UTXO age, so a quote of the same transaction arms the stop; audit 2026-10-29 F1) | NTV04n2, NTV04n3, NTW04n2, NTW04n3 | NTV04n1(F) NTV04n2 NTV04n3 NTW04n1(F) NTW04n2 NTW04n3 | CONFIRMED |
| TV-active | KobCondBid | touch (update: arm on a bid, trail on an ask): exposure counts the evidence activeFrom | NTV03, NTW03 | NTV03 NTW03 | CONFIRMED |
| TV-tokdaa | KobCondBid | touch (update: arm on a bid, trail on an ask): exposure counts the evidence ask custody DAA | NTW02 | NTW02 | CONFIRMED |
| TV-tkcov | KobCondBid | touch (update: arm on a bid, trail on an ask): the custody input tk carries the token covenant id | NTW02b | NTW02b | CONFIRMED |
| TV-owner | KobCondBid | touch (update: arm on a bid, trail on an ask): the custody input tk is owned by the evidence ask (owner = its covenant id, scheme covenant id) | NTW15, NTW16 | NTW15 NTW16 | CONFIRMED |
| TV-tktpl | KobCondBid | touch (update: arm on a bid, trail on an ask): the custody input tk is a holder UTXO of the token's program (template hash; P2SH kept) | NTW02c | NTW02c(F) | CONFIRMED |
| TV-tkp2sh | KobCondBid | touch (update: arm on a bid, trail on an ask): the custody input tk is the P2SH of the holder redeem script read (template hash kept) | NTW02d | NTW02d(F) | CONFIRMED |
| TV-bidext | KobCondBid | touch (update: arm on a bid, trail on an ask): the evidence bid names the order's extension commitment (KobBid extensionCommitment [118..150)) | NAUD-EXT-CB1 | NAUD-EXT-CB1 | CONFIRMED |
| TV-ext | KobCondBid | touch (update: arm on a bid, trail on an ask): the evidence ask's custody tk is of the order's extension commitment (units of the covenant id of another commitment are another token) | NAUD-EXT-CB2 | NAUD-EXT-CB2 | CONFIRMED |
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
| TK-cltv | KobIfdAsk | touchAsk (sell-stop entry): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTK01, NTK02, NTK03, NTK04, NTL01, NTL02, NTL03, NTL04 | NTK01 NTK02 NTK03 NTK04 NTK04n4(F) NTL01 NTL02 NTL03 NTL04 NTL04n4(F) | CONFIRMED |
| TK-interval | KobIfdAsk | touchAsk (sell-stop entry): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTK04, NTL04 | NTK04 NTL04 | CONFIRMED |
| TK-ivpos | KobIfdAsk | touchAsk (sell-stop entry): the evidence interval is not negative (a negative one cancels the UTXO age, so a quote of the same transaction arms the stop; audit 2026-10-29 F1) | NTK04n2, NTK04n3, NTL04n2, NTL04n3 | NTK04n1(F) NTK04n2 NTK04n3 NTL04n1(F) NTL04n2 NTL04n3 | CONFIRMED |
| TK-active | KobIfdAsk | touchAsk (sell-stop entry): exposure counts the evidence activeFrom | NTK03, NTL03 | NTK03 NTL03 | CONFIRMED |
| TK-tokdaa | KobIfdAsk | touchAsk (sell-stop entry): exposure counts the evidence ask custody DAA | NTK02, NTL02 | NTK02 NTL02 | CONFIRMED |
| TK-tkcov | KobIfdAsk | touchAsk (sell-stop entry): the custody input tk carries the token covenant id | NTK02b, NTL02b | NTK02b NTL02b | CONFIRMED |
| TK-owner | KobIfdAsk | touchAsk (sell-stop entry): the custody input tk is owned by the evidence ask (owner = its covenant id, scheme covenant id) | NTK15, NTK16, NTL15, NTL16 | NTK15 NTK16 NTL15 NTL16 | CONFIRMED |
| TK-tktpl | KobIfdAsk | touchAsk (sell-stop entry): the custody input tk is a holder UTXO of the token's program (template hash; P2SH kept) | NTK02c, NTL02c | NTK02c(F) NTL02c(F) | CONFIRMED |
| TK-tkp2sh | KobIfdAsk | touchAsk (sell-stop entry): the custody input tk is the P2SH of the holder redeem script read (template hash kept) | NTK02d, NTL02d | NTK02d(F) NTL02d(F) | CONFIRMED |
| TK-ext | KobIfdAsk | touchAsk (sell-stop entry): the evidence ask's custody tk is of the order's extension commitment (units of the covenant id of another commitment are another token) | NAUD-EXT-IA1, NAUD-EXT-IA2 | NAUD-EXT-IA1 NAUD-EXT-IA2(F) | CONFIRMED |
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
| AU-CB-1 | KobCondBid | tplState: the evidence input is the P2SH of the redeem script it pushes (the generated TB-p2sh / TV-p2sh entries swap the call; this removes the check in the shared function) | NTB18, NTV18, NTW18 | NTB18 NTV18 NTW02d(F) NTW18 | CONFIRMED |
| AU-CB-2 | KobCondBid | tplState: the evidence's template is the KobAsk / KobBid template (the check in the shared function) | NTB17, NTV17, NTW17 | NTB17 NTV17 NTW02c(F) NTW17 | CONFIRMED |
| AU-CB-3 | KobCondBid | noStrays bound: at most MAX_TOK_IN = 8 token inputs (a 9th is not scanned; the KCC-20 program refuses a 9th input on its own, so only the order flips) | NAU-CB30 | NAU-CB30(F) | CONFIRMED |
| AU-CB-S0 | KobCondBid | noStrays scan slot 0: a stray of the order at token slot 0 rides to the seller | NAU-CBS0 | NAU-CBS0 | CONFIRMED |
| AU-CB-S1 | KobCondBid | noStrays scan slot 1: a stray of the order at token slot 1 rides to the seller | NAU-CBS1 | NAU-CBS1 | CONFIRMED |
| AU-CB-S2 | KobCondBid | noStrays scan slot 2: a stray of the order at token slot 2 rides to the seller | NAU-CBS2 | NAU-CBS2 | CONFIRMED |
| AU-CB-S3 | KobCondBid | noStrays scan slot 3: a stray of the order at token slot 3 rides to the seller | NAU-CBS3 | NAU-CBS3 | CONFIRMED |
| AU-CB-S4 | KobCondBid | noStrays scan slot 4: a stray of the order at token slot 4 rides to the seller | NAU-CBS4 | NAU-CBS4 | CONFIRMED |
| AU-CB-S5 | KobCondBid | noStrays scan slot 5: a stray of the order at token slot 5 rides to the seller | NAU-CBS5 | NAU-CBS5 | CONFIRMED |
| AU-CB-S6 | KobCondBid | noStrays scan slot 6: a stray of the order at token slot 6 rides to the seller | NAU-CBS6 | NAU-CBS6 | CONFIRMED |
| AU-CB-S7 | KobCondBid | noStrays scan slot 7: a stray of the order at token slot 7 rides to the seller | NAU-CBS7 | NAU-CBS7 | CONFIRMED |
| AU-CB-4 | KobCondBid | settle: the order is the only input of its covenant id (a sibling UTXO of the id beside the fill) | NAU-CB01 | NAU-CB01 | CONFIRMED |
| AU-CB-5 | KobCondBid | settle: no fill before activeFrom | NAU-CB02 | NAU-CB02 | CONFIRMED |
| AU-CB-6 | KobCondBid | settle: n > 0 (with a maker-set minFill 0 the minFill rule admits n = 0: an empty delivery drains deliveryCarrier per call) | NAU-CB03 | NAU-CB03 | CONFIRMED |
| AU-CB-7 | KobCondBid | settle: n <= amountLeft (a fill of 5 from an order of 4) | NB14 | NB14 | CONFIRMED |
| AU-CB-8 | KobCondBid | settle: scale > 0 (hostile field: at scale -1000 the spend turns negative, the seller funds the delivery) | NAU-CB04 | NAU-CB04 | CONFIRMED |
| AU-CB-9 | KobCondBid | settle: tip >= 0 (hostile field: a negative tip pays below the leg price) | NAU-CB05 | NAU-CB05 | CONFIRMED |
| AU-CB-10 | KobCondBid | settle: deliveryCarrier >= 0 (hostile field: the delivery carries nothing) | NAU-CB06 | NAU-CB06 | CONFIRMED |
| AU-CB-11 | KobCondBid | settle, limit leg: tpPrice > 0 (a stop-only order filled on its limit leg at price 0) | NB6 | NB6 | CONFIRMED |
| AU-CB-12 | KobCondBid | settle: leg is 0 or 1 (leg 2 would run as the stop leg) | NAU-CB07 | NAU-CB07 | CONFIRMED |
| AU-CB-13 | KobCondBid | settle, stop leg: stopPrice > 0 (a limit-only order filled on a stop leg at price 0) | NAU-CB08 | NAU-CB08 | CONFIRMED |
| AU-CB-14 | KobCondBid | settle, stop leg: slipBps >= 0 (hostile field: a negative band) | NAU-CB09 | NAU-CB09 | CONFIRMED |
| AU-CB-15 | KobCondBid | settle, stop leg: slipBps <= 10000 (a band above 100%) | NB26 | NB26 | CONFIRMED |
| AU-CB-16 | KobCondBid | settle, armed auction: tx.daa >= t (CLTV; a future t raises the buy price) | NAU-CB10 | NAU-CB10 | CONFIRMED |
| AU-CB-17 | KobCondBid | settle, stop leg: stopPrice <= MAX_STOP (the band product stopPrice * 10000 fits; with band 0 a larger stop fills) | NAU-CB11 | NAU-CB11 | CONFIRMED |
| AU-CB-18 | KobCondBid | re-arm: output i pays the maker's profit to P2PK(maker) | NAU-CB12 | NAU-CB12 | CONFIRMED |
| AU-CB-19 | KobCondBid | delivery: the template source input carries the token covenant id (same-program bytes of another covenant are hash-identical; the check pins the source; now in tokRs, shared with the mode-1 refund) | NAU-CB13 | NAU-CB13 | CONFIRMED |
| AU-CB-20 | KobCondBid | delivery: the template bytes hash to the token's template (a planted look-alike would give the maker an anyone-can-spend fake; now in tokRs, shared with the mode-1 refund) | NAU-CB14 | NAU-CB14 | CONFIRMED |
| AU-CB-21 | KobCondBid | delivery: output i is the holder of exactly n base units owned by the maker | NB18 | NB18 | CONFIRMED |
| AU-CB-22 | KobCondBid | delivery: bound to the token covenant (an unbound copy of the holder script delivers nothing) | NAU-CB15 | NAU-CB15 | CONFIRMED |
| AU-CB-23 | KobCondBid | partial fill: exactly one output bound to the order's id | NAU-CB16 | NAU-CB16 | CONFIRMED |
| AU-CB-24 | KobCondBid | partial fill: the continuation is this script with amountLeft - n and the new armed state | NB15, NB16 | NB15 NB16 | CONFIRMED |
| AU-CB-25 | KobCondBid | partial fill: the continuation keeps the escrow minus the spend and the delivery carrier | NB1, NB4 | NB1 NB4 | CONFIRMED |
| AU-CB-26 | KobCondBid | partial fill: the delivery carries deliveryCarrier | NB2 | NB2 | CONFIRMED |
| AU-CB-27 | KobCondBid | termination: no output bound to the order's id | NAU-CB17 | NAU-CB17 | CONFIRMED |
| AU-CB-28 | KobCondBid | termination below REFUND_MIN: everything not spent rides on the delivery | NV3CB10 | NV3CB10 | CONFIRMED |
| AU-CB-29 | KobCondBid | update: the order is the only input of its covenant id | NAU-CB18 | NAU-CB18 | CONFIRMED |
| AU-CB-30 | KobCondBid | update: stopPrice > 0 (a limit-only order armed: the keeper is paid for nothing) | NAU-CB19 | NAU-CB19 | CONFIRMED |
| AU-CB-31 | KobCondBid | update: not before activeFrom | NAU-CB20 | NAU-CB20 | CONFIRMED |
| AU-CB-32 | KobCondBid | update: exactly one output bound to the order's id | NAU-CB21 | NAU-CB21 | CONFIRMED |
| AU-CB-33 | KobCondBid | update: the continuation is this script with the justified stop and armed state | NB21, NB21b | NB21 NB21b | CONFIRMED |
| AU-CB-34 | KobCondBid | update: scale > 0. DEFENCE IN DEPTH: the evidence must quote at the same (negative) scale, and the evidence KobAsk refuses its own negative scale (NAU-CB22, judged at the evidence input); AU-CB-34x removes both | stay rejected: NAU-CB22 |  | held (defence in depth) |
| AU-CB-34x | KobCondBid | update scale > 0 and the evidence KobAsk's scale > 0 removed together: at scale -1000 the keeper takes keeperTip out of the buying budget | NAU-CB22 | NAU-CB22 | CONFIRMED |
| AU-CB-35 | KobCondBid | update: the continuation keeps the buying budget (the keeper's tip only from the excess, at most keeperTip) | NB25, NB30, NB30b | NB25 NB30b NB30 | CONFIRMED |
| AU-CB-36 | KobCondBid | cancel: SIGHASH_ALL only | NAU-CB23 | NAU-CB23 | CONFIRMED |
| AU-CB-37 | KobCondBid | cancel: the maker's signature | NAU-CB24 | NAU-CB24 | CONFIRMED |
| AU-CB-38 | KobCondBid | refund only once due: min(expiryDaa, 90 days idle) | NAU-CB25 | NAU-CB25 | CONFIRMED |
| AU-CB-39 | KobCondBid | refund: the order is the only input of its covenant id | NAU-CB26 | NAU-CB26 | CONFIRMED |
| AU-CB-40 | KobCondBid | refund: no token input (strays owned by the id stay put) | NS11 | NS11 | CONFIRMED |
| AU-CB-41 | KobCondBid | refund: no output bound to the order's id | NAU-CB27 | NAU-CB27 | CONFIRMED |
| AU-CB-42 | KobCondBid | refund: output i pays P2PK(maker) | NAU-CB28 | NAU-CB28 | CONFIRMED |
| AU-CB-43 | KobCondBid | refund: the maker gets the escrow minus at most refundTip | NAU-CB29 | NAU-CB29 | CONFIRMED |
| AU-IA-1 | KobIfdAsk | tplRs: the custody input is the P2SH of the holder script it pushes (a planted UTXO of the token covenant id) | NAU-IA01 | NAU-IA01 | CONFIRMED |
| AU-IA-2 | KobIfdAsk | tplRs: the custody's template is the token's (a planted P2SH of a look-alike template) | NAU-IA02 | NAU-IA02 | CONFIRMED |
| AU-IA-3 | KobIfdAsk | tplState: the evidence input is the P2SH of the redeem script it pushes (the check in the shared function) | NTK18, NTL18 | NTK02d(F) NTK18 NTL02d(F) NTL18 | CONFIRMED |
| AU-IA-4 | KobIfdAsk | tplState: the evidence's template is the KobAsk template (the check in the shared function) | NTK17, NTL17 | NTK02c(F) NTK17 NTL02c(F) NTL17 | CONFIRMED |
| AU-IA-5 | KobIfdAsk | noStrays bound: at most MAX_TOK_IN = 8 token inputs (a 9th is not scanned; the KCC-20 program refuses a 9th input on its own, so only the entry flips) | NAU-IA37 | NAU-IA37(F) | CONFIRMED |
| AU-IA-S0 | KobIfdAsk | noStrays scan slot 0: a stray of the entry at token slot 0 rides to the taker | NAU-IAS0 | NAU-IAS0 | CONFIRMED |
| AU-IA-S1 | KobIfdAsk | noStrays scan slot 1: a stray of the entry at token slot 1 rides to the taker | NAU-IAS1 | NAU-IAS1 | CONFIRMED |
| AU-IA-S2 | KobIfdAsk | noStrays scan slot 2: a stray of the entry at token slot 2 rides to the taker | NAU-IAS2 | NAU-IAS2 | CONFIRMED |
| AU-IA-S3 | KobIfdAsk | noStrays scan slot 3: a stray of the entry at token slot 3 rides to the taker | NAU-IAS3 | NAU-IAS3 | CONFIRMED |
| AU-IA-S4 | KobIfdAsk | noStrays scan slot 4: a stray of the entry at token slot 4 rides to the taker | NAU-IAS4 | NAU-IAS4 | CONFIRMED |
| AU-IA-S5 | KobIfdAsk | noStrays scan slot 5: a stray of the entry at token slot 5 rides to the taker | NAU-IAS5 | NAU-IAS5 | CONFIRMED |
| AU-IA-S6 | KobIfdAsk | noStrays scan slot 6: a stray of the entry at token slot 6 rides to the taker | NAU-IAS6 | NAU-IAS6 | CONFIRMED |
| AU-IA-S7 | KobIfdAsk | noStrays scan slot 7: a stray of the entry at token slot 7 rides to the taker | NAU-IAS7 | NAU-IAS7 | CONFIRMED |
| AU-IA-6 | KobIfdAsk | settle: the entry is the only input of its covenant id | NAU-IA03 | NAU-IA03 | CONFIRMED |
| AU-IA-7 | KobIfdAsk | settle: the custody carries the token covenant id (not a UTXO of another token of the same program owned by the entry) | NAU-IA04 | NAU-IA04 | CONFIRMED |
| AU-IA-8 | KobIfdAsk | settle: the custody is owned by this entry's covenant id (not tokens of another id: the real custody would be orphaned) | NAU-IA05 | NAU-IA05 | CONFIRMED |
| AU-IA-9 | KobIfdAsk | settle: the custody has owner scheme covenant id and borrowing disabled (0x040100) | NAU-IA06 | NAU-IA06 | CONFIRMED |
| AU-IA-10 | KobIfdAsk | settle: the custody carries the committed exit's extension commitment (another commitment is another token) | NAU-IA07 | NAU-IA07 | CONFIRMED |
| AU-IA-11 | KobIfdAsk | refund (nb = 0) only once due: min(expiryDaa, 90 days idle) | NAU-IA08 | NAU-IA08 | CONFIRMED |
| AU-IA-12 | KobIfdAsk | refund: the maker gets the carriers minus at most refundTip | NAU-IA09 | NAU-IA09 | CONFIRMED |
| AU-IA-13 | KobIfdAsk | merge: scale > 0. DEFENCE IN DEPTH: the merged exit quotes at the entry scale (checked), and that KobCondBid refuses its own negative scale (NAU-IA36, judged at the exit input); AU-IA-13x removes both | stay rejected: NAU-IA36 |  | held (defence in depth) |
| AU-IA-13x | KobIfdAsk | merge scale > 0 and the exit's scale > 0 removed together: an entry at scale -1000 merges its booked exit | NAU-IA36 | NAU-IA36 | CONFIRMED |
| AU-IA-14 | KobIfdAsk | merge: m > 0. DEFENCE IN DEPTH: m = 0 needs the exit at input k to settle n = 0, which the exit refuses (n > 0; NAU-IA10, judged at the exit input); AU-IA-14x removes both | stay rejected: NAU-IA10 |  | held (defence in depth) |
| AU-IA-14x | KobIfdAsk | merge m > 0 and the exit's n > 0 removed together: a no-op merge of m = 0 (argument -(1 * 2^53)) | NAU-IA10 | NAU-IA10 | CONFIRMED |
| AU-IA-15 | KobIfdAsk | merge: the exit terms slipBps .. minRestDaa (state [231..286)) are the committed exitState | NAU-IA11 | NAU-IA11 | CONFIRMED |
| AU-IA-15b | KobIfdAsk | merge: the push opcode of the exit armed field (state 294) is the committed one (a look-alike exit pushing armed as OP_DATA_7 + OP_NOP runs) | NAU-IA38 | NAU-IA38 | CONFIRMED |
| AU-IA-16 | KobIfdAsk | merge: the exit terms bandDaa / keeperTip (state [303..321)) are the committed exitState | NAU-IA12 | NAU-IA12 | CONFIRMED |
| AU-IA-17 | KobIfdAsk | fill: no fill before activeFrom | NAU-IA13 | NAU-IA13 | CONFIRMED |
| AU-IA-18 | KobIfdAsk | fill: scale > 0 (hostile field: at scale -1000 the proceeds turn negative, the taker takes KAS from the entry) | NAU-IA14 | NAU-IA14 | CONFIRMED |
| AU-IA-19 | KobIfdAsk | fill: tip >= 0 (hostile field) | NI12 | NI12 | CONFIRMED |
| AU-IA-20 | KobIfdAsk | fill: prefund >= 0 (hostile field: the exit gets less than its proceeds) | NAU-IA15 | NAU-IA15 | CONFIRMED |
| AU-IA-21 | KobIfdAsk | fill: exitCarrier >= 0 (hostile field) | NAU-IA16 | NAU-IA16 | CONFIRMED |
| AU-IA-22 | KobIfdAsk | fill: n <= amountLeft (11 whole sold from a 10-whole custody) | NAU-IA17 | NAU-IA17 | CONFIRMED |
| AU-IA-23 | KobIfdAsk | fill: a stop entry has entryStop >= price | NI21 | NI21 | CONFIRMED |
| AU-IA-24 | KobIfdAsk | fill, armed auction: tx.daa >= t (CLTV; a future t lowers the sell price) | NAU-IA18 | NAU-IA18 | CONFIRMED |
| AU-IA-25 | KobIfdAsk | fill, armed auction: t >= origin (a time before the origin is refused; it could only raise the price) | NAU-IA19 | NAU-IA19 | CONFIRMED |
| AU-IA-26 | KobIfdAsk | booking: a fill takes at most rptAmount - 1 while a minimum fill of re-arms is left | NRPB6b | NRPB6b | CONFIRMED |
| AU-IA-27 | KobIfdAsk | the exit template hashes to COND_BID_TPL (an anyone-can-spend template around the exit state); holds NI10 (another template under a KobCondBid output is also refused by the spk pin) | NAU-IA20; stay rejected: NI10 | NAU-IA20 | CONFIRMED |
| AU-IA-28 | KobIfdAsk | the exit output is exactly the built exit script. DEFENCE IN DEPTH: the genesis id is recomputed from the built script too, so another script fails it; AU-IA-28x removes both | stay rejected: NI1, NI2, NI2b |  | held (defence in depth) |
| AU-IA-29 | KobIfdAsk | the exit is the fresh one-output genesis authorised by this input (a two-output group, another authoriser, a continuation of an attacker covenant); holds NI1 / NI2 / NI2b (the spk pin) | NI3, NI3b, NI5; stay rejected: NI1, NI2, NI2b | NI3 NI3b NI5 | CONFIRMED |
| AU-IA-28x | KobIfdAsk | the exit spk pin and the genesis id removed together: a tampered exit (amountLeft 5 for 4, the stop removed, a wider band) is created | NI1, NI2, NI2b | NI1 NI2 NI2b NI3 NI3b NI5 | CONFIRMED |
| AU-IA-30 | KobIfdAsk | partial fill: the rest custody keeps its carrier | NAU-IA21 | NAU-IA21 | CONFIRMED |
| AU-IA-31 | KobIfdAsk | partial fill: the exit holds the proceeds, the prefund and exitCarrier | NI6, NV3IA10 | NI6 NV3IA10 NV3IA11 | CONFIRMED |
| AU-IA-32 | KobIfdAsk | sold out: the last exit takes the proceeds and everything left | NI7 | NI7 | CONFIRMED |
| AU-IA-33 | KobIfdAsk | continuation: exactly one output bound to the entry's id | NAU-IA22 | NAU-IA22 | CONFIRMED |
| AU-IA-34 | KobIfdAsk | continuation: holds at least its floor | NI8 | NI8 | CONFIRMED |
| AU-IA-35 | KobIfdAsk | termination: no output bound to the entry's id | NAU-IA23 | NAU-IA23 | CONFIRMED |
| AU-IA-36 | KobIfdAsk | the one token output (rest, refund, merged custody) is pinned: the custody template with the expected state | NI9 | NI9 | CONFIRMED |
| AU-IA-37 | KobIfdAsk | the pinned token output is bound to the token covenant (an unbound copy of the holder script would let the taker take the rest) | NAU-IA24 | NAU-IA24 | CONFIRMED |
| AU-IA-38 | KobIfdAsk | close only once due: min(expiryDaa, 90 days idle) | NRPB27 | NRPB27 | CONFIRMED |
| AU-IA-39 | KobIfdAsk | close: the entry is the only input of its covenant id | NAU-IA25 | NAU-IA25 | CONFIRMED |
| AU-IA-40 | KobIfdAsk | close: no output bound to the entry's id | NAU-IA26 | NAU-IA26 | CONFIRMED |
| AU-IA-41 | KobIfdAsk | close: output i pays P2PK(maker) | SR1e | SR1e | CONFIRMED |
| AU-IA-42 | KobIfdAsk | close: the maker gets the carrier minus at most refundTip | NAU-IA27 | NAU-IA27 | CONFIRMED |
| AU-IA-43 | KobIfdAsk | update: the entry is the only input of its covenant id | NAU-IA28 | NAU-IA28 | CONFIRMED |
| AU-IA-44 | KobIfdAsk | update: entryStop > 0 (a limit entry at price 0 armed next to an ask fill quoting 0: the keeper is paid for nothing) | NAU-IA29 | NAU-IA29 | CONFIRMED |
| AU-IA-45 | KobIfdAsk | update: only while unarmed (a re-arm pays the keeper again) | NAU-IA30 | NAU-IA30 | CONFIRMED |
| AU-IA-46 | KobIfdAsk | update: not before activeFrom | NAU-IA31 | NAU-IA31 | CONFIRMED |
| AU-IA-47 | KobIfdAsk | update: exactly one output bound to the entry's id | NAU-IA32 | NAU-IA32 | CONFIRMED |
| AU-IA-48 | KobIfdAsk | update: the continuation is this script, armed (not a repriced entry) | NAU-IA33 | NAU-IA33 | CONFIRMED |
| AU-IA-49 | KobIfdAsk | update: the keeper takes at most keeperTip | NI23 | NI23 | CONFIRMED |
| AU-IA-50 | KobIfdAsk | cancel: SIGHASH_ALL only | NAU-IA34 | NAU-IA34 | CONFIRMED |
| AU-IA-51 | KobIfdAsk | cancel: the maker's signature | NAU-IA35 | NAU-IA35 | CONFIRMED |
| AU-CB-44 | KobCondBid | update: keeperTip >= 0 (hostile field: a negative keeperTip, its floor clamped to the escrow, would arm as keeperTip 0; as M4 / NK2 for KobCondAsk) | NAU-CB31 | NAU-CB31 | CONFIRMED |
| AU-IA-52 | KobIfdAsk | update: keeperTip >= 0 (hostile field: the keeper tops the entry up; as M4 / NK2 for KobCondAsk) | NAU-IA39 | NAU-IA39 | CONFIRMED |
| BRC-CB-spk | KobCondBid | refundTo: the slot pays P2PK(maker), the destination of refund() (a redirected, P2SH or omitted refund; there the slot holds the taker's change) | BRC21, BRC22, BRC23, BRC25, BRC27, BRC29, BRC32 | BRC21 BRC22 BRC23 BRC25 BRC27 BRC29 BRC32 | CONFIRMED |
| BRC-CB-value | KobCondBid | refundTo: the slot holds at least the remainder (one sompi short, an empty or smaller refund at the slot) | BRC24, BRC33, BRC34 | BRC24 BRC33 BRC34 | CONFIRMED |
| BRC-CB-alias | KobCondBid | refundTo aliasing guard: the slot is not the positional output of another order / router input (an ask of the same maker paying its proceeds there, any other covenant input) | BRC37, BRC38 | BRC37 BRC38 | CONFIRMED |
| BRC-CB-slot | KobCondBid | refundTo: one slot per order input (M - 1 - self): without the - self two orders of one maker share the last output | BRC32, BRC33 | BRC32 BRC33 | CONFIRMED [positive scenarios rejected: BRC07+] |
| BRC-CB-carrier | KobCondBid | terminal fill with a refund: the delivery still carries deliveryCarrier | BRC26 | BRC26 | CONFIRMED |
| BRC-CB-branch | KobCondBid | the branch condition flipped (rem < REFUND_MIN): a remainder of REFUND_MIN or more rides on the delivery again, one below it needs the refund layout | BRC25, BRC27, BRC28 | BRC25 BRC27 BRC28 BRC36(F) | CONFIRMED [positive scenarios rejected: BRC01+ BRC02+ BRC03+ BRC04+ BRC05+ BRC06+ BRC07+ BRC08+ BRC09+ BRC10+ BRC11+] |
| BRC-CB-above | KobCondBid | the boundary moved up (rem > REFUND_MIN): a remainder of exactly REFUND_MIN rides on the delivery | BRC27 | BRC27 | CONFIRMED [positive scenarios rejected: BRC02+] |
| BRC-CB-below | KobCondBid | the boundary moved down (rem >= REFUND_MIN - 1): a remainder of REFUND_MIN - 1 sompi is split off to the maker and the delivery keeps only the carrier | BRC28 | BRC28 | CONFIRMED [positive scenarios rejected: BRC03+] |
| DM-CB-mode-range | KobCondBid | delivery modes (bid-delivery-spec): mode <= 2 | DM2-01 | DM2-01 | CONFIRMED |
| DM-CB-mode-nonneg | KobCondBid | delivery modes (bid-delivery-spec): mode >= 0 | DM2-02 | DM2-02 | CONFIRMED |
| DM-CB-bought-zero | KobCondBid | delivery modes (bid-delivery-spec): bought == 0 unless mode 1 | DM2-03 | DM2-03 DM2-04 | CONFIRMED |
| DM-CB-bought-nonneg | KobCondBid | delivery modes (bid-delivery-spec): bought >= 0 | DM2-05 | DM2-05(F) | CONFIRMED |
| DM-CB-form-range | KobCondBid | delivery modes (bid-delivery-spec): mode 2: form in {0, 1, 2} | DM2-06 | DM2-06 DM2-07 | CONFIRMED |
| DM-CB-recv-thr-pos | KobCondBid | delivery modes (bid-delivery-spec): mode 2: recvThreshold > 0 | DM2-08 | DM2-08 | CONFIRMED |
| DM-CB-tip-cap | KobCondBid | delivery modes (bid-delivery-spec): mode 2: topupTip <= deliveryCarrier | DM2-09 | DM2-09 | CONFIRMED |
| DM-CB-tip-nonneg | KobCondBid | delivery modes (bid-delivery-spec): mode 2: topupTip >= 0 | DM2-10 | DM2-10 | CONFIRMED |
| DM-CB-d-kas | KobCondBid | delivery modes (bid-delivery-spec): mode 1 custody / new delivery: the delivery output carries haveKas + onDelivery | DM2-17 | DM2-12 DM2-17 DM2-33(F) | CONFIRMED |
| DM-CB-topup-owner | KobCondBid | delivery modes (bid-delivery-spec): top-up: the receive UTXO is the maker key, scheme 0x00, borrow 0x01 | DM2-24 | DM2-24(F) DM2-28 | CONFIRMED |
| DM-CB-topup-thr | KobCondBid | delivery modes (bid-delivery-spec): top-up: the receive UTXO threshold >= recvThreshold | DM2-25 | DM2-25 | CONFIRMED |
| DM-CB-topup-guard | KobCondBid | delivery modes (bid-delivery-spec): top-up: canonical guard (zero bytes after the threshold) | DM2-26 | DM2-26 | CONFIRMED |
| DM-CB-topup-ext | KobCondBid | delivery modes (bid-delivery-spec): top-up: the receive UTXO carries the order extension commitment | DM2-27 | DM2-27(F) | CONFIRMED |
| DM-CB-topup-p2sh | KobCondBid | delivery modes (bid-delivery-spec): top-up: tokIn is the P2SH of the script its state is read from | DM2-30 | DM2-30(F) | CONFIRMED |
| DM-CB-rem-nonneg | KobCondBid | delivery modes (bid-delivery-spec): terminal fill: rem + onDelivery >= 0 | DM2-34 | DM2-34 | CONFIRMED |
| DM-CB-topup-succ | KobCondBid | delivery modes (bid-delivery-spec): top-up: output i is the receive UTXO's successor (DLV-07: a second order claims the receive UTXO another order tops up) | DM2-36 | DM2-36 | CONFIRMED |
| DM-CB-is-custody | KobCondBid | delivery modes (bid-delivery-spec): mode 1 later fill: tokIn is the order bought-custody | DM3-02 | DM3-02 DM3-03(F) DM3-04 DM3-05(F) | CONFIRMED |
| DM-CB-stray-excl | KobCondBid | delivery modes (bid-delivery-spec): stray guard: the custody is excluded only when bought > 0 | DM3-07 | DM3-07 | CONFIRMED |
| DM-CB-rearm-bought | KobCondBid | delivery modes (bid-delivery-spec): a re-arming fill needs bought == 0 | DM8-08 | DM8-08(F) | CONFIRMED |
| DM-CB-refund-count | KobCondBid | delivery modes (bid-delivery-spec): refund with a custody: one token input of the token | DM6-06 | DM6-06 | CONFIRMED |
| DM-CB-refund-iscust | KobCondBid | delivery modes (bid-delivery-spec): refund: custIn is the bought-custody | DM6-07 | DM6-07 | CONFIRMED |
| DM-CB-refund-kas | KobCondBid | delivery modes (bid-delivery-spec): refund: the custody output keeps its KAS | DM6-04 | DM6-04 | CONFIRMED |
| DM-CB-refund-zero | KobCondBid | delivery modes (bid-delivery-spec): refund with bought 0: no token input of the token (strays) | DM6-09 | DM6-09 | CONFIRMED |
| DM-CB-update-strays | KobCondBid | delivery modes (bid-delivery-spec): update: no stray of the order | DM4-04 | DM4-04 | CONFIRMED |
| DM-CB-iscust-p2sh | KobCondBid | isCustody: the bought-custody is the P2SH of the script it pushes (a planted non-P2SH input of the token's covenant id with the genuine custody redeem; the token program also refuses it on its own input) | DM3-05 | DM3-05(F) | CONFIRMED |
| DM-CB-iscust-state | KobCondBid | isCustody (DM3-03: the token program also refuses the mix of commitments on its own input): the custody is known by its exact state (amount == bought, owner the id, scheme 0x04, borrowing disabled, zero guard, the commitment), in a fill and in the refund | DM3-02, DM3-03, DM3-04, DM6-07 | DM3-02 DM3-03(F) DM3-04 DM6-07 | CONFIRMED |
| DM-CB-refund-pin | KobCondBid | refund: the custody goes whole to the maker's key as one plain UTXO (not another key, not kept under the id, not borrow-enabled; DM6-05, the release at another index, is also refused by the release's KAS check at the custody index) | DM6-01, DM6-02, DM6-03 | DM6-01 DM6-02 DM6-03 | CONFIRMED |
| DM-CB-refund-cov | KobCondBid | refund: the release is bound to the token covenant (an unbound copy of the release script at the custody's index holds nothing; the custody goes to the taker) | DM6-10 | DM6-10 | CONFIRMED |
| DM-IA-rpt-mode | KobIfdAsk | delivery modes (bid-delivery-spec): KobIfdAsk: a repeating entry commits exitMode 0 | DM8-05 | DM8-05 DM8-06 | CONFIRMED |
| DM-IA-merge-tail | KobIfdAsk | delivery modes (bid-delivery-spec): KobIfdAsk merge: the merged exit carries the entry delivery tail | DM8-07 | DM8-07 | CONFIRMED |

Collateral flips (scenarios accepted besides the expected ones):

- C3b: NRPB14
- D3: NRPB31
- D4: NRPB16c
- D5: NRPB17
- D6: NRPB3 NRPB19b NRPB19c NRPB19d NRPB35
- D7b: NRPB24b
- FXL3d: NRPB20
- TB-push08: NTB11
- TB-cltv: NTB04n4
- TB-ivpos: NTB04n1
- TV-cltv: NTV04n4 NTW04n4
- TV-ivpos: NTV04n1 NTW04n1
- TK-cltv: NTK04n4 NTL04n4
- TK-ivpos: NTK04n1 NTL04n1
- IA-proceeds-ceil: NV3IA11
- AU-CB-1: NTW02d
- AU-CB-2: NTW02c
- AU-IA-3: NTK02d NTL02d
- AU-IA-4: NTK02c NTL02c
- AU-IA-28x: NI3 NI3b NI5
- AU-IA-31: NV3IA11
- BRC-CB-branch: BRC36
- DM-CB-bought-zero: DM2-04
- DM-CB-form-range: DM2-07
- DM-CB-d-kas: DM2-12 DM2-33
- DM-CB-topup-owner: DM2-28
- DM-CB-is-custody: DM3-03 DM3-04 DM3-05
- DM-IA-rpt-mode: DM8-06

## kron-sell

KRON adapter (protocol v3, sell-side contracts); test binary `kob_kron_v2_tests`; contracts `contracts/orders/kron`. 320/320 confirmed, 6/6 defence-in-depth held (expected NOT to flip): the full run had 319/320; AU-KB-4 was repaired and re-run.
Baseline (no mutation): v2_lifecycle_auctions_and_kill clean, v2_kron_v23_extra_attacks clean, v2_lifecycle_stray_custody clean, v2_lifecycle_stop_auction_keeper_trailing clean, v2_negative_trigger_manipulation clean, v2_lifecycle_ifd_stop_entry clean, v3_min_fill clean, v2_repeat_ifd_attacks clean, v3_merge_push clean, v2_kron_fix_pass_regressions clean, v2_touch_ifd_arm clean, v2_touch_cond_settle clean, v2_touch_cond_arm clean, v2_touch_cond_trail clean, v3_rounding clean, v3_tip clean, v3_merge_limits clean, v3_audit_ask_kron clean, v2_kron_attacks_ask_side clean, v2_negative_tif_cancel_expiry clean, v2_touch_ifd_fill clean, v2_negative_limits_and_tips clean, v2_negative_time_variants clean, v2_kron_attacks_bid_side clean, v3_audit_bid_kron clean, v3_bid_refund_kron clean, v2_kron_attacks_conditional_and_ifd clean, v3_audit_cond_ask_kron clean, v2_negative_oco_and_ifd clean, v3_audit_ifd_bid_kron clean.

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
| TA-cltv | KobCondAskKron | touchAsk (settle, stop leg): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTA01, NTA02, NTA03, NTA04 | NTA01 NTA02 NTA03 NTA04 NTA04n4(F) | NTA01 NTA02 NTA03 NTA04 NTA04n4(F) | CONFIRMED |
| TA-interval | KobCondAskKron | touchAsk (settle, stop leg): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTA04 | NTA04 | NTA04 | CONFIRMED |
| TA-ivpos | KobCondAskKron | touchAsk (settle, stop leg): the evidence interval is not negative (a negative one cancels the UTXO age, so a quote of the same transaction arms the stop; audit 2026-10-29 F1) | NTA04n2, NTA04n3 | NTA04n1(F) NTA04n2 NTA04n3 | NTA04n1(F) NTA04n2 NTA04n3 | CONFIRMED |
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
| TU-cltv | KobCondAskKron | touch (update: arm on an ask, trail on a bid): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTU01, NTU02, NTU03, NTU04, NTT01, NTT03, NTT04 | NTU01 NTU02 NTU03 NTU04 NTU04n4(F) NTT01 NTT03 NTT04 NTT04n4(F) | NTU01 NTU02 NTU03 NTU04 NTU04n4(F) NTT01 NTT03 NTT04 NTT04n4(F) | CONFIRMED |
| TU-interval | KobCondAskKron | touch (update: arm on an ask, trail on a bid): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTU04, NTT04 | NTU04 NTT04 | NTU04 NTT04 | CONFIRMED |
| TU-ivpos | KobCondAskKron | touch (update: arm on an ask, trail on a bid): the evidence interval is not negative (a negative one cancels the UTXO age, so a quote of the same transaction arms the stop; audit 2026-10-29 F1) | NTU04n2, NTU04n3, NTT04n2, NTT04n3 | NTU04n1(F) NTU04n2 NTU04n3 NTT04n1(F) NTT04n2 NTT04n3 | NTU04n1(F) NTU04n2 NTU04n3 NTT04n1(F) NTT04n2 NTT04n3 | CONFIRMED |
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
| TI-cltv | KobIfdBidKron | touchBid (buy-stop entry): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTI01, NTI03, NTI04, NTJ01, NTJ03, NTJ04 | NTI01 NTI03 NTI04 NTI04n4(F) NTJ01 NTJ03 NTJ04 NTJ04n4(F) | NTI01 NTI03 NTI04 NTI04n4(F) NTJ01 NTJ03 NTJ04 NTJ04n4(F) | CONFIRMED |
| TI-interval | KobIfdBidKron | touchBid (buy-stop entry): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTI04, NTJ04 | NTI04 NTJ04 | NTI04 NTJ04 | CONFIRMED |
| TI-ivpos | KobIfdBidKron | touchBid (buy-stop entry): the evidence interval is not negative (a negative one cancels the UTXO age, so a quote of the same transaction arms the stop; audit 2026-10-29 F1) | NTI04n2, NTI04n3, NTJ04n2, NTJ04n3 | NTI04n1(F) NTI04n2 NTI04n3 NTJ04n1(F) NTJ04n2 NTJ04n3 | NTI04n1(F) NTI04n2 NTI04n3 NTJ04n1(F) NTJ04n2 NTJ04n3 | CONFIRMED |
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
| V3-B-minfill | KobBidKron | minFill: n >= minFill unless the fill takes all the buying power left (less than one base unit funded after it) | V3B01, V3B08, V3B08b, V3B09 | V3B01 V3B08 V3B08b V3B09 | V3B01 V3B08 V3B08b V3B09 | CONFIRMED |
| V3-B-minfill-end | KobBidKron | the pre-fix rule (exempt whenever the bid ends, less than one minimum fill left) lets a small fill end a bid funded for under two minimum fills (KobBid 242aaae3 on testnet-10) | V3B08, V3B08b, V3B09 | V3B08 V3B08b V3B09 | V3B08 V3B08b V3B09 | CONFIRMED |
| V3-B-minfill-one | KobBidKron | the exemption is strict: a fill that leaves exactly one base unit of buying power is not exempt (< -> <=) | V3B09 | V3B09 | V3B09 | CONFIRMED |
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
| AU-KA-1 | KobAskKron | tplRs: the custody input is the P2SH of the holder script it pushes (a planted UTXO of the token covenant id) | NAU-KA03 | NAU-KA03 | NAU-KA03 | CONFIRMED |
| AU-KA-2 | KobAskKron | tplRs: the custody's template is the token's (a planted P2SH of a look-alike template) | NAU-KA04 | NAU-KA04 | NAU-KA04 | CONFIRMED |
| AU-KA-3 | KobAskKron | noStrays bound: at most MAX_TOK_IN = 4 token inputs (a 5th is not scanned; the KRON program refuses a 5th input on its own, so only the ask flips) | NAU-KA22 | NAU-KA22(F) | NAU-KA22(F) | CONFIRMED |
| AU-KA-S0 | KobAskKron | noStrays scan slot 0: a stray of the ask at token slot 0 rides to the taker | NAU-KAS0 | NAU-KAS0 | NAU-KAS0 | CONFIRMED |
| AU-KA-S1 | KobAskKron | noStrays scan slot 1: a stray of the ask at token slot 1 rides to the taker | NAU-KAS1 | NAU-KAS1 | NAU-KAS1 | CONFIRMED |
| AU-KA-S2 | KobAskKron | noStrays scan slot 2: a stray of the ask at token slot 2 rides to the taker | NAU-KAS2 | NAU-KAS2 | NAU-KAS2 | CONFIRMED |
| AU-KA-S3 | KobAskKron | noStrays scan slot 3: a stray of the ask at token slot 3 rides to the taker | NAU-KAS3 | NAU-KAS3 | NAU-KAS3 | CONFIRMED |
| AU-KA-4 | KobAskKron | the ask is the only input of its covenant id (a sibling UTXO of the id beside the fill) | NAU-KA01 | NAU-KA01 | NAU-KA01 | CONFIRMED |
| AU-KA-5 | KobAskKron | the custody carries the token covenant id (not a UTXO of another token of the same program owned by the ask) | NAU-KA02 | NAU-KA02 | NAU-KA02 | CONFIRMED |
| AU-KA-6 | KobAskKron | the custody is owned by this ask's covenant id (not tokens of another id: the ask's real custody would be orphaned) | NAU-KA05 | NAU-KA05 | NAU-KA05 | CONFIRMED |
| AU-KA-7 | KobAskKron | the custody has id_type 2 (covenant id): a custody of the order id with id_type 3 is also refused by the token program (no input is P2PK of the id), so only the ask flips | KA4b | KA4b(F) | KA4b(F) | CONFIRMED |
| AU-KA-8 | KobAskKron | the custody is no minter (is_minter 0) | KA4c | KA4c | KA4c | CONFIRMED |
| AU-KA-9 | KobAskKron | refund only once due: min(expiryDaa, 90 days idle, IOC / FOK kill) | NC8, NC9, NC8b, NC3, NC4 | NC8 NC9 NC8b NC3 NC4 | NC8 NC9 NC8b NC3 NC4 | CONFIRMED |
| AU-KA-10 | KobAskKron | a refund leaves no output bound to the ask's covenant id (a keeper's UTXO of the id would own every later stray) | NAU-KA08 | NAU-KA08 | NAU-KA08 | CONFIRMED |
| AU-KA-11 | KobAskKron | refund: the maker gets the carriers minus at most refundTip | NC6 | NC6 | NC6 | CONFIRMED |
| AU-KA-12 | KobAskKron | no fill before activeFrom | NT-ACT | NT-ACT | NT-ACT | CONFIRMED |
| AU-KA-13 | KobAskKron | fill n > 0 (NAU-KA10, n = -1, is also refused by n >= minFill \|\| n == amountLeft; AU-KA-13x removes both) | stay rejected: NAU-KA10 |  |  | held (defence in depth) |
| AU-KA-13x | KobAskKron | n > 0 and the minFill rule removed together: a fill of n = -1 (the taker adds a unit, the maker is paid nothing) | NAU-KA10 | NAU-KA10 | NAU-KA10 | CONFIRMED |
| AU-KA-14 | KobAskKron | scale > 0 (hostile field: an ask at a negative scale sells for nothing) | NAU-KA11 | NAU-KA11 | NAU-KA11 | CONFIRMED |
| AU-KA-15 | KobAskKron | tip >= 0 (hostile field) | NF7 | NF7 | NF7 | CONFIRMED |
| AU-KA-16 | KobAskKron | n <= amountLeft: a fill cannot claim more than the ask holds (an evidence fill of 1 base unit claiming minTouch would arm a stop) | NAU-KA09 | NAU-KA09 | NAU-KA09 | CONFIRMED |
| AU-KA-17 | KobAskKron | TWAP: the UTXO is >= interval DAA old (CSV) | NV1 | NV1 | NV1 | CONFIRMED |
| AU-KA-18 | KobAskKron | TWAP: n <= maxFill | NV2 | NV2 | NV2 | CONFIRMED |
| AU-KA-19 | KobAskKron | decay: slope > 0 (a rising ask would reward understated time) | NV7 | NV7 | NV7 | CONFIRMED |
| AU-KA-20 | KobAskKron | positional payout: output i pays P2PK(maker) | NF9 | NF9 | NF9 | CONFIRMED |
| AU-KA-21 | KobAskKron | GTC rest: exactly one output bound to the ask's id (no second UTXO of the id for the taker) | NAU-KA14 | NAU-KA14 | NAU-KA14 | CONFIRMED |
| AU-KA-22 | KobAskKron | GTC rest: the continuation keeps the order's carrier | NF10 | NF10 | NF10 | CONFIRMED |
| AU-KA-23 | KobAskKron | GTC rest: the rest custody keeps its carrier | KA7 | KA7 | KA7 | CONFIRMED |
| AU-KA-24 | KobAskKron | partial fill: the maker gets at least the all-in proceeds | NF1, NF2, NV6, V3A02 | NF1 NF2 NV6 V3A02 | NF1 NF2 NV6 V3A02 | CONFIRMED |
| AU-KA-25 | KobAskKron | FOK: a fill that leaves tokens unsold is refused | NAU-KA17 | NAU-KA17 | NAU-KA17 | CONFIRMED |
| AU-KA-26 | KobAskKron | termination (IOC, sold out): no output bound to the ask's id | NAU-KA18 | NAU-KA18 | NAU-KA18 | CONFIRMED |
| AU-KA-27 | KobAskKron | IOC return: the returned rest keeps the custody's carrier | NAU-KA19 | NAU-KA19 | NAU-KA19 | CONFIRMED |
| AU-KA-28 | KobAskKron | IOC: the maker gets the all-in proceeds plus the order carrier | NM1 | NM1 | NM1 | CONFIRMED |
| AU-KA-29 | KobAskKron | sold out: the maker gets the all-in proceeds plus both carriers | NAU-KA20 | NAU-KA20 | NAU-KA20 | CONFIRMED |
| AU-KA-30 | KobAskKron | the one token output (rest, IOC return, refund) is pinned: the custody template with the expected state (owner, id_type; the minter byte is also refused by the token program) | NT-IOC2, KA1, KA2, KA2b, KA13, KA13b, KA13c | NT-IOC2 KA1 KA2 KA2b KA3(F) KA-IOC(F) KA13 KA13b KA13c KA13d(F) | NT-IOC2 KA1 KA2 KA2b KA3(F) KA-IOC(F) KA13 KA13b KA13c KA13d(F) | CONFIRMED |
| AU-KA-31 | KobAskKron | the pinned token output is bound to the token covenant (an unbound copy of the holder script would let the taker take the rest) | NAU-KA21 | NAU-KA21 | NAU-KA21 | CONFIRMED |
| AU-KA-32 | KobAskKron | cancel: SIGHASH_ALL only | NC1 | NC1 | NC1 | CONFIRMED |
| AU-KA-33 | KobAskKron | cancel: the maker's signature | NC2 | NC2 | NC2 | CONFIRMED |
| AU-KB-1 | KobBidKron | noStrays bound: at most MAX_TOK_IN = 4 token inputs (a 5th is not scanned; the KRON program refuses a 5th input on its own, so only the bid flips) | NAU-KB26 | NAU-KB26(F) | NAU-KB26(F) | CONFIRMED |
| AU-KB-S0 | KobBidKron | noStrays scan slot 0: a stray of the bid at token slot 0 rides to the taker | NAU-KBS0 | NAU-KBS0 | NAU-KBS0 | CONFIRMED |
| AU-KB-S1 | KobBidKron | noStrays scan slot 1: a stray of the bid at token slot 1 rides to the taker | NAU-KBS1 | NAU-KBS1 | NAU-KBS1 | CONFIRMED |
| AU-KB-S2 | KobBidKron | noStrays scan slot 2: a stray of the bid at token slot 2 rides to the taker | NAU-KBS2 | NAU-KBS2 | NAU-KBS2 | CONFIRMED |
| AU-KB-S3 | KobBidKron | noStrays scan slot 3: a stray of the bid at token slot 3 rides to the taker | NAU-KBS3 | NAU-KBS3 | NAU-KBS3 | CONFIRMED |
| AU-KB-2 | KobBidKron | the bid is the only input of its covenant id (a sibling UTXO of the id beside the fill) | NAU-KB01 | NAU-KB01 | NAU-KB01 | CONFIRMED |
| AU-KB-3 | KobBidKron | no fill before activeFrom | NAU-KB02 | NAU-KB02 | NAU-KB02 | CONFIRMED |
| AU-KB-4 | KobBidKron | fill n > 0 (a zero fill would close an exhausted bid before its expiry; the KRON program also refuses the empty delivery on its own input, so only the bid flips) | NAU-KB03 | NAU-KB03(F) | NAU-KB03(F) | CONFIRMED (re-run after the scenario fix, see the 2026-11-24 section) |
| AU-KB-5 | KobBidKron | scale > 0 (hostile field: a negative scale turns the quote negative) | NAU-KB04 | NAU-KB04 | NAU-KB04 | CONFIRMED |
| AU-KB-6 | KobBidKron | tip >= 0 (hostile field: the bid would pay below its quoted price) | NAU-KB05 | NAU-KB05 | NAU-KB05 | CONFIRMED |
| AU-KB-7 | KobBidKron | reserve >= 0 (hostile field: a negative reserve lets an underfunded bid fill) | NAU-KB06 | NAU-KB06 | NAU-KB06 | CONFIRMED |
| AU-KB-8 | KobBidKron | deliveryCarrier >= 0 (hostile field) | NAU-KB07 | NAU-KB07 | NAU-KB07 | CONFIRMED |
| AU-KB-9 | KobBidKron | DCA: the UTXO is >= interval DAA old (CSV) | NV3 | NV3 | NV3 | CONFIRMED |
| AU-KB-10 | KobBidKron | DCA: n <= maxFill | NV4 | NV4 | NV4 | CONFIRMED |
| AU-KB-11 | KobBidKron | rising: slope > 0 (a falling bid would reward understated time) | NAU-KB08 | NAU-KB08 | NAU-KB08 | CONFIRMED |
| AU-KB-12 | KobBidKron | rising: tx.daa >= t (CLTV proves the time) | NV9 | NV9 | NV9 | CONFIRMED |
| AU-KB-13 | KobBidKron | rising: t >= origin (a time before the origin is refused; it could only keep or lower the price) | NAU-KB10 | NAU-KB10 | NAU-KB10 | CONFIRMED |
| AU-KB-14 | KobBidKron | a fill never spends the reserve: left >= reserve | NAU-KB11 | NAU-KB11 | NAU-KB11 | CONFIRMED |
| AU-KB-15 | KobBidKron | the delivery template is read from a token input of this token (the bytes are also hash-pinned, so a same-program source of another covenant gains nothing; the check pins the source) | NAU-KB12 | NAU-KB12 | NAU-KB12 | CONFIRMED |
| AU-KB-16 | KobBidKron | the delivery template's hash is the token's (a planted look-alike would give the maker an anyone-can-spend fake) | NAU-KB13 | NAU-KB13 | NAU-KB13 | CONFIRMED |
| AU-KB-17 | KobBidKron | positional delivery: output i is the holder of exactly n base units owned by the maker with id_type 3 (the minter byte is also refused by the token program) | KB1, KB2, KB2b | KB1 KB2 KB2b KB3(F) KB4 | KB1 KB2 KB2b KB3(F) KB4 | CONFIRMED |
| AU-KB-18 | KobBidKron | the delivery is bound to the token covenant (an unbound copy of the holder script delivers nothing) | NAU-KB15 | NAU-KB15 | NAU-KB15 | CONFIRMED |
| AU-KB-19 | KobBidKron | GTC continuation: exactly one output bound to the bid's id | NAU-KB16 | NAU-KB16 | NAU-KB16 | CONFIRMED |
| AU-KB-20 | KobBidKron | GTC continuation: this script (not a bid of the taker) | NAU-KB17 | NAU-KB17 | NAU-KB17 | CONFIRMED |
| AU-KB-21 | KobBidKron | GTC continuation: exactly the unused escrow | NF5, NF6 | NF5 NF6 | NF5 NF6 | CONFIRMED |
| AU-KB-22 | KobBidKron | GTC: the delivery carries the carrier plus the budget not spent | NF4, V3B05 | NF4 V3B05 | NF4 V3B05 | CONFIRMED |
| AU-KB-23 | KobBidKron | FOK: a fill that leaves buying power is refused | NT-FOK2 | NT-FOK2 | NT-FOK2 | CONFIRMED |
| AU-KB-24 | KobBidKron | termination: no output bound to the bid's id | NAU-KB18, NBR15 | NAU-KB18 NBR15 | NAU-KB18 NBR15 | CONFIRMED |
| AU-KB-25 | KobBidKron | termination without a refund (rem < REFUND_MIN): everything not spent rides on the delivery | NF3, NV8, NBR08 | NF3 NV8 NBR07 NBR08 | NF3 NV8 NBR07 NBR08 | CONFIRMED |
| AU-KB-26 | KobBidKron | cancel: SIGHASH_ALL only | NAU-KB24 | NAU-KB24 | NAU-KB24 | CONFIRMED |
| AU-KB-27 | KobBidKron | cancel: the maker's signature | NAU-KB25 | NAU-KB25 | NAU-KB25 | CONFIRMED |
| AU-KB-28 | KobBidKron | refund only once due: min(expiryDaa, 90 days idle, IOC / FOK kill) | NC10, NC10b, NC3b, NC5 | NC10 NC10b NC3b NC5 | NC10 NC10b NC3b NC5 | CONFIRMED |
| AU-KB-29 | KobBidKron | refund: the bid is the only input of its covenant id | NAU-KB19 | NAU-KB19 | NAU-KB19 | CONFIRMED |
| AU-KB-30 | KobBidKron | refund: no output bound to the bid's id | NAU-KB21 | NAU-KB21 | NAU-KB21 | CONFIRMED |
| AU-KB-31 | KobBidKron | refund: output i pays P2PK(maker) | NC7, NAU-KB22 | NC7 NAU-KB22 | NC7 NAU-KB22 | CONFIRMED |
| AU-KB-32 | KobBidKron | refund: the maker gets the escrow minus at most refundTip | NAU-KB23 | NAU-KB23 | NAU-KB23 | CONFIRMED |
| AU-KCA-1 | KobCondAskKron | tplRs: the custody input is the P2SH of the holder script it pushes (a planted UTXO of the token covenant id) | NAU-KCA03 | NAU-KCA03 | NAU-KCA03 | CONFIRMED |
| AU-KCA-2 | KobCondAskKron | tplRs: the custody's template is the token's (a planted P2SH of a look-alike template) | NAU-KCA04 | NAU-KCA04 | NAU-KCA04 | CONFIRMED |
| AU-KCA-3 | KobCondAskKron | tplState (evidence and its custody): the input is the P2SH of the redeem script it pushes (the real evidence script pushed by a non-P2SH input) | NTA18, NTU18, NTT18 | NTA02d(F) NTA18 NTU02d(F) NTU18 NTT18 | NTA02d(F) NTA18 NTU02d(F) NTU18 NTT18 | CONFIRMED |
| AU-KCA-4 | KobCondAskKron | tplState (evidence and its custody): the template hash (a look-alike evidence order's state under another template) | NT6, NTA17, NTU17, NTT17 | NT6 NTA02c(F) NTA17 NTU02c(F) NTU17 NTT17 | NT6 NTA02c(F) NTA17 NTU02c(F) NTU17 NTT17 | CONFIRMED |
| AU-KCA-5 | KobCondAskKron | noStrays bound: at most MAX_TOK_IN = 4 token inputs (a 5th is not scanned; the KRON program refuses a 5th input on its own, so only the order flips) | NAU-KCA39 | NAU-KCA39(F) | NAU-KCA39(F) | CONFIRMED |
| AU-KCA-S0 | KobCondAskKron | noStrays scan slot 0: a stray of the order at token slot 0 rides to the taker | NAU-KCAS0 | NAU-KCAS0 | NAU-KCAS0 | CONFIRMED |
| AU-KCA-S1 | KobCondAskKron | noStrays scan slot 1: a stray of the order at token slot 1 rides to the taker | NAU-KCAS1 | NAU-KCAS1 | NAU-KCAS1 | CONFIRMED |
| AU-KCA-S2 | KobCondAskKron | noStrays scan slot 2: a stray of the order at token slot 2 rides to the taker | NAU-KCAS2 | NAU-KCAS2 | NAU-KCAS2 | CONFIRMED |
| AU-KCA-S3 | KobCondAskKron | noStrays scan slot 3: a stray of the order at token slot 3 rides to the taker | NAU-KCAS3 | NAU-KCAS3 | NAU-KCAS3 | CONFIRMED |
| AU-KCA-6 | KobCondAskKron | settle: the order is the only input of its covenant id (a sibling UTXO of the id beside the fill) | NAU-KCA01 | NAU-KCA01 | NAU-KCA01 | CONFIRMED |
| AU-KCA-7 | KobCondAskKron | the custody carries the token covenant id (not a UTXO of another token of the same program owned by the order) | NAU-KCA02 | NAU-KCA02 | NAU-KCA02 | CONFIRMED |
| AU-KCA-8 | KobCondAskKron | the custody is owned by this order's covenant id | NAU-KCA05 | NAU-KCA05 | NAU-KCA05 | CONFIRMED |
| AU-KCA-9 | KobCondAskKron | the custody has id_type 2 (covenant id): a custody of the order id with id_type 3 is also refused by the token program (no input is P2PK of the id), so only the order flips | NAU-KCA06 | NAU-KCA06(F) | NAU-KCA06(F) | CONFIRMED |
| AU-KCA-10 | KobCondAskKron | the custody is no minter (is_minter 0) | KC4 | KC4 | KC4 | CONFIRMED |
| AU-KCA-11 | KobCondAskKron | refund only from min(expiryDaa, 90 days idle) | NAU-KCA08 | NAU-KCA08 | NAU-KCA08 | CONFIRMED |
| AU-KCA-12 | KobCondAskKron | a refund leaves no output bound to the order's id | NAU-KCA09 | NAU-KCA09 | NAU-KCA09 | CONFIRMED |
| AU-KCA-13 | KobCondAskKron | refund: the maker gets the carriers minus at most refundTip | NAU-KCA10 | NAU-KCA10 | NAU-KCA10 | CONFIRMED |
| AU-KCA-14 | KobCondAskKron | no fill before activeFrom | NAU-KCA11 | NAU-KCA11 | NAU-KCA11 | CONFIRMED |
| AU-KCA-15 | KobCondAskKron | fill n > 0 (NAU-KCA19, n = -1, is also refused by n >= minFill \|\| n == amountLeft; AU-KCA-15x removes both) | stay rejected: NAU-KCA19 |  |  | held (defence in depth) |
| AU-KCA-15x | KobCondAskKron | n > 0 and the minFill rule removed together: a fill of n = -1 (the taker adds a unit, the maker is paid nothing) | NAU-KCA19 | NAU-KCA19 | NAU-KCA19 | CONFIRMED |
| AU-KCA-16 | KobCondAskKron | scale > 0 (hostile field: a negative scale sells for nothing) | NAU-KCA12 | NAU-KCA12 | NAU-KCA12 | CONFIRMED |
| AU-KCA-17 | KobCondAskKron | tip >= 0 (hostile field) | NAU-KCA13 | NAU-KCA13 | NAU-KCA13 | CONFIRMED |
| AU-KCA-18 | KobCondAskKron | n <= amountLeft (a fill claiming more than the custody holds) | NAU-KCA22 | NAU-KCA22 | NAU-KCA22 | CONFIRMED |
| AU-KCA-19 | KobCondAskKron | the TP leg needs tpPrice > 0 (a stop-only order would sell on the TP leg for nothing) | NAU-KCA23 | NAU-KCA23 | NAU-KCA23 | CONFIRMED |
| AU-KCA-20 | KobCondAskKron | leg is 0 or 1 | NAU-KCA24 | NAU-KCA24 | NAU-KCA24 | CONFIRMED |
| AU-KCA-21 | KobCondAskKron | the stop leg needs stopPrice > 0 | NAU-KCA25 | NAU-KCA25 | NAU-KCA25 | CONFIRMED |
| AU-KCA-22 | KobCondAskKron | slipBps >= 0 (hostile field) | NAU-KCA26 | NAU-KCA26 | NAU-KCA26 | CONFIRMED |
| AU-KCA-23 | KobCondAskKron | slipBps <= 10000 (hostile field: a band wider than the stop price; outside an auction the leg price would also go negative) | NAU-KCA27 | NAU-KCA27 | NAU-KCA27 | CONFIRMED |
| AU-KCA-24 | KobCondAskKron | stopPrice <= MAX_STOP (the band product must fit; with slipBps 0 an oversized stop would sell) | NAU-KCA28 | NAU-KCA28 | NAU-KCA28 | CONFIRMED |
| AU-KCA-25 | KobCondAskKron | positional payout: output i pays P2PK(maker) | NAU-KCA17 | NAU-KCA17 | NAU-KCA17 | CONFIRMED |
| AU-KCA-26 | KobCondAskKron | rest: exactly one output bound to the order's id | NAU-KCA14 | NAU-KCA14 | NAU-KCA14 | CONFIRMED |
| AU-KCA-27 | KobCondAskKron | rest: the continuation keeps the order's carrier | NAU-KCA29 | NAU-KCA29 | NAU-KCA29 | CONFIRMED |
| AU-KCA-28 | KobCondAskKron | rest: the rest custody keeps its carrier | NAU-KCA16 | NAU-KCA16 | NAU-KCA16 | CONFIRMED |
| AU-KCA-29 | KobCondAskKron | sold out: no output bound to the order's id | NAU-KCA18 | NAU-KCA18 | NAU-KCA18 | CONFIRMED |
| AU-KCA-30 | KobCondAskKron | re-arming sell-out: the maker gets the profit (all-in minus the budget) | NAU-KCA31 | NAU-KCA31 | NAU-KCA31 | CONFIRMED |
| AU-KCA-31 | KobCondAskKron | sold out: the maker gets the all-in proceeds plus both carriers | NAU-KCA20 | NAU-KCA20 | NAU-KCA20 | CONFIRMED |
| AU-KCA-32 | KobCondAskKron | the one token output (rest, refund) is pinned (template, amount, owner, id_type; the minter byte is also refused by the token program) | NAU-KCA30, KC1, KC2 | NAU-KCA30 KC1 KC2 KC3(F) | NAU-KCA30 KC1 KC2 KC3(F) | CONFIRMED |
| AU-KCA-33 | KobCondAskKron | the pinned token output is bound to the token covenant | NAU-KCA21 | NAU-KCA21 | NAU-KCA21 | CONFIRMED |
| AU-KCA-34 | KobCondAskKron | update: the order is the only input of its covenant id | NAU-KCA32 | NAU-KCA32 | NAU-KCA32 | CONFIRMED |
| AU-KCA-35 | KobCondAskKron | update: stopPrice > 0 (an order without a stop leg is never armed or trailed) | NAU-KCA33 | NAU-KCA33 | NAU-KCA33 | CONFIRMED |
| AU-KCA-36 | KobCondAskKron | update: not before activeFrom | NAU-KCA34 | NAU-KCA34 | NAU-KCA34 | CONFIRMED |
| AU-KCA-37 | KobCondAskKron | update: exactly one output bound to the order's id | NAU-KCA35 | NAU-KCA35 | NAU-KCA35 | CONFIRMED |
| AU-KCA-38 | KobCondAskKron | update: the continuation changes only stopPrice / armed as the evidence justifies | NAU-KCA36 | NAU-KCA36 | NAU-KCA36 | CONFIRMED |
| AU-KCA-39 | KobCondAskKron | cancel: SIGHASH_ALL only | NAU-KCA37 | NAU-KCA37 | NAU-KCA37 | CONFIRMED |
| AU-KCA-40 | KobCondAskKron | cancel: the maker's signature | NAU-KCA38 | NAU-KCA38 | NAU-KCA38 | CONFIRMED |
| AU-KIB-1 | KobIfdBidKron | tplState (evidence bid, merged exit): the input is the P2SH of the redeem script it pushes | NTI18, NTJ18 | NTI18 NTJ18 | NTI18 NTJ18 | CONFIRMED |
| AU-KIB-2 | KobIfdBidKron | tplState (evidence bid, merged exit): the template hash (a look-alike evidence bid) | NTI17, NTJ17 | NTI17 NTJ17 | NTI17 NTJ17 | CONFIRMED |
| AU-KIB-3 | KobIfdBidKron | noStrays bound: at most MAX_TOK_IN = 4 token inputs (a 5th is not scanned; the KRON program refuses a 5th input on its own, so only the entry flips) | NAU-KIB30 | NAU-KIB30(F) | NAU-KIB30(F) | CONFIRMED |
| AU-KIB-S0 | KobIfdBidKron | noStrays scan slot 0: a stray of the entry at token slot 0 rides to the taker | NAU-KIBS0 | NAU-KIBS0 | NAU-KIBS0 | CONFIRMED |
| AU-KIB-S1 | KobIfdBidKron | noStrays scan slot 1: a stray of the entry at token slot 1 rides to the taker | NAU-KIBS1 | NAU-KIBS1 | NAU-KIBS1 | CONFIRMED |
| AU-KIB-S2 | KobIfdBidKron | noStrays scan slot 2: a stray of the entry at token slot 2 rides to the taker | NAU-KIBS2 | NAU-KIBS2 | NAU-KIBS2 | CONFIRMED |
| AU-KIB-S3 | KobIfdBidKron | noStrays scan slot 3: a stray of the entry at token slot 3 rides to the taker | NAU-KIBS3 | NAU-KIBS3 | NAU-KIBS3 | CONFIRMED |
| AU-KIB-4 | KobIfdBidKron | fill / merge: the entry is the only input of its covenant id | NAU-KIB01 | NAU-KIB01 | NAU-KIB01 | CONFIRMED |
| AU-KIB-5 | KobIfdBidKron | scale > 0 (hostile field: a negative scale turns the spend negative) | NAU-KIB02 | NAU-KIB02 | NAU-KIB02 | CONFIRMED |
| AU-KIB-6 | KobIfdBidKron | tip >= 0 (hostile field) | NAU-KIB03 | NAU-KIB03 | NAU-KIB03 | CONFIRMED |
| AU-KIB-7 | KobIfdBidKron | merge: the exit matches the committed exitState on [189..244) (slipBps .. minRestDaa) | NAU-KIB04 | NAU-KIB04 | NAU-KIB04 | CONFIRMED |
| AU-KIB-8 | KobIfdBidKron | merge: the exit matches the committed exitState on [252..271) (bandDaa, keeperTip) | NAU-KIB05 | NAU-KIB05 | NAU-KIB05 | CONFIRMED |
| AU-KIB-9 | KobIfdBidKron | no fill before activeFrom | NAU-KIB07 | NAU-KIB07 | NAU-KIB07 | CONFIRMED |
| AU-KIB-10 | KobIfdBidKron | fill n > 0 (a zero fill of an empty repeating entry books an empty exit; the KRON program also refuses the empty delivery on its own input, so only the entry flips) | NAU-KIB08 | NAU-KIB08(F) | NAU-KIB08(F) | CONFIRMED |
| AU-KIB-11 | KobIfdBidKron | n <= amountLeft | NI10 | NI10 | NI10 | CONFIRMED |
| AU-KIB-12 | KobIfdBidKron | deliveryCarrier >= 0 (hostile field) | NAU-KIB09 | NAU-KIB09 | NAU-KIB09 | CONFIRMED |
| AU-KIB-13 | KobIfdBidKron | exitCarrier >= 0 (hostile field) | NAU-KIB10 | NAU-KIB10 | NAU-KIB10 | CONFIRMED |
| AU-KIB-14 | KobIfdBidKron | stop entry auction: tx.daa >= t (CLTV proves the auction time) | NAU-KIB31 | NAU-KIB31 | NAU-KIB31 | CONFIRMED |
| AU-KIB-15 | KobIfdBidKron | stop entry auction: t >= the auction origin | NAU-KIB32 | NAU-KIB32 | NAU-KIB32 | CONFIRMED |
| AU-KIB-16 | KobIfdBidKron | repeat: while a minimum fill of re-arms is left, a fill takes at most that many (and is booked) | NAU-KIB33 | NAU-KIB33 | NAU-KIB33 | CONFIRMED |
| AU-KIB-17 | KobIfdBidKron | the exit template is KobCondAskKron (COND_TPL): a look-alike template around the committed state would make the exit and its tokens anyone-can-spend (NI4, the KobAskKron template beside a KobCondAskKron output, is also refused by the exit script equality) | NAU-KIB34; stay rejected: NI4 | NAU-KIB34 | NAU-KIB34 | CONFIRMED |
| AU-KIB-18 | KobIfdBidKron | the exit is exactly the committed KobCondAskKron with amountLeft := n (defence in depth: the exit covenant id is derived from exitSpk, so a tampered exit output also fails the genesis id check; AU-KIB-18x removes both) | stay rejected: NI2, NI2b |  |  | held (defence in depth) |
| AU-KIB-19 | KobIfdBidKron | the exit is a fresh genesis covenant created by this input (NI7, the entry's own id, is also refused by the continuation count) | NI6, NI8; stay rejected: NI7 | NI6 NI8 | NI6 NI8 | CONFIRMED |
| AU-KIB-18x | KobIfdBidKron | the exit script equality and the genesis id check removed together: a tampered exit is accepted | NI2 | NI2 NI2b NI6 NI8 | NI2 NI2b NI6 NI8 | CONFIRMED |
| AU-KIB-20 | KobIfdBidKron | continuing fill: the exit holds at least exitCarrier | NI5c | NI5c | NI5c | CONFIRMED |
| AU-KIB-21 | KobIfdBidKron | final fill: the exit gets everything left | NI5 | NI5 | NI5 | CONFIRMED |
| AU-KIB-22 | KobIfdBidKron | the delivery template is read from a token input of this token | NAU-KIB12 | NAU-KIB12 | NAU-KIB12 | CONFIRMED |
| AU-KIB-23 | KobIfdBidKron | the delivery template's hash is the token's (a planted look-alike would give the exit an anyone-can-spend fake) | NAU-KIB13 | NAU-KIB13 | NAU-KIB13 | CONFIRMED |
| AU-KIB-24 | KobIfdBidKron | positional delivery: output i holds exactly n base units owned by the exit's id with id_type 2 (the minter byte is also refused by the token program) | NI1, NI3, KI1, KI2 | NI1 NI3 KI1 KI2 KI3(F) | NI1 NI3 KI1 KI2 KI3(F) | CONFIRMED |
| AU-KIB-25 | KobIfdBidKron | the delivery is bound to the token covenant | NAU-KIB14 | NAU-KIB14 | NAU-KIB14 | CONFIRMED |
| AU-KIB-26 | KobIfdBidKron | the delivery carries at least deliveryCarrier | NAU-KIB15 | NAU-KIB15 | NAU-KIB15 | CONFIRMED |
| AU-KIB-27 | KobIfdBidKron | continuation: exactly one output bound to the entry's id | NAU-KIB11 | NAU-KIB11 | NAU-KIB11 | CONFIRMED |
| AU-KIB-28 | KobIfdBidKron | continuation: holds at least the floor | NI5b | NI5b | NI5b | CONFIRMED |
| AU-KIB-29 | KobIfdBidKron | termination: no output bound to the entry's id | NAU-KIB16 | NAU-KIB16 | NAU-KIB16 | CONFIRMED |
| AU-KIB-30 | KobIfdBidKron | update: the entry is the only input of its covenant id | NAU-KIB17 | NAU-KIB17 | NAU-KIB17 | CONFIRMED |
| AU-KIB-31 | KobIfdBidKron | update: only an unarmed entry (re-arming an armed auction entry would restart its auction) | NAU-KIB18 | NAU-KIB18 | NAU-KIB18 | CONFIRMED |
| AU-KIB-32 | KobIfdBidKron | update: not before activeFrom | NAU-KIB20 | NAU-KIB20 | NAU-KIB20 | CONFIRMED |
| AU-KIB-33 | KobIfdBidKron | update: exactly one output bound to the entry's id | NAU-KIB21 | NAU-KIB21 | NAU-KIB21 | CONFIRMED |
| AU-KIB-34 | KobIfdBidKron | cancel: SIGHASH_ALL only | NAU-KIB22 | NAU-KIB22 | NAU-KIB22 | CONFIRMED |
| AU-KIB-35 | KobIfdBidKron | cancel: the maker's signature | NAU-KIB23 | NAU-KIB23 | NAU-KIB23 | CONFIRMED |
| AU-KIB-36 | KobIfdBidKron | refund only from min(expiryDaa, 90 days idle) | NAU-KIB24 | NAU-KIB24 | NAU-KIB24 | CONFIRMED |
| AU-KIB-37 | KobIfdBidKron | refund: the entry is the only input of its covenant id | NAU-KIB25 | NAU-KIB25 | NAU-KIB25 | CONFIRMED |
| AU-KIB-38 | KobIfdBidKron | refund: no output bound to the entry's id | NAU-KIB27 | NAU-KIB27 | NAU-KIB27 | CONFIRMED |
| AU-KIB-39 | KobIfdBidKron | refund: output i pays P2PK(maker) | NAU-KIB28 | NAU-KIB28 | NAU-KIB28 | CONFIRMED |
| AU-KIB-40 | KobIfdBidKron | refund: the maker gets the escrow minus at most refundTip | NAU-KIB29 | NAU-KIB29 | NAU-KIB29 | CONFIRMED |
| BR-1 | KobBidKron | refundTo aliasing guard: the input at the refund slot is a token input or a plain key input, not another order (a KobAskKron of the same maker whose payout doubles as the refund) | NBR14 | NBR14 | NBR14 | CONFIRMED |
| BR-2 | KobBidKron | refundTo: the output at M - 1 - self pays P2PK(maker) (another key, a non-P2PK script, no refund output, one output for two bids, swapped refunds, a slot off by one) | NBR01, NBR02, NBR04, NBR09, NBR10, NBR11, NBR12 | NBR01 NBR02 NBR04 NBR06 NBR09 NBR10 NBR11 NBR12 | NBR01 NBR02 NBR04 NBR06 NBR09 NBR10 NBR11 NBR12 | CONFIRMED |
| BR-3 | KobBidKron | refundTo: the refund carries at least rem (1 sompi short is refused; a spare output after the refund is also refused by the P2PK check) | NBR03, NM4; stay rejected: NBR13 | NBR03 NM4 | NBR03 NM4 | CONFIRMED |
| BR-3x | KobBidKron | refundTo: P2PK(maker) and value >= rem removed together: a slot one past the refund (a spare output) is accepted | NBR03, NBR13 | NBR01 NBR02 NBR03 NBR04 NBR06 NBR09 NBR10 NBR11 NBR12 NBR13 | NBR01 NBR02 NBR03 NBR04 NBR06 NBR09 NBR10 NBR11 NBR12 NBR13 | CONFIRMED |
| BR-4 | KobBidKron | terminal fill: refundTo is called at all (every refund-layout attack that keeps the delivery at deliveryCarrier or more is accepted) | NBR01, NBR02, NBR03, NBR04, NBR09, NBR10, NBR11, NBR12, NBR13, NBR14 | NBR01 NBR02 NBR03 NBR04 NBR06 NBR09 NBR10 NBR11 NBR12 NBR13 NBR14 | NBR01 NBR02 NBR03 NBR04 NBR06 NBR09 NBR10 NBR11 NBR12 NBR13 NBR14 | CONFIRMED |
| BR-5 | KobBidKron | terminal fill with a refund: the delivery keeps deliveryCarrier (1 sompi below it, refund 1 sompi above rem) | NBR05 | NBR05 | NBR05 | CONFIRMED |
| BR-7 | KobBidKron | the refund branch is entered only by the condition: if (false) always takes the delivery-only branch, so a missing refund at rem 6 KAS is accepted | NBR04 | NBR04 NBR06 | NBR04 NBR06 | CONFIRMED [positive scenarios rejected: PBR01 PBR02 PBR03 PBR04 PBR06 PBR07 PBR08 PBR09 PBR10 PBR11 PBR12] |
| BR-8 | KobBidKron | threshold boundary: rem >= REFUND_MIN becomes rem > REFUND_MIN, so rem exactly REFUND_MIN may stay on the delivery | NBR06 | NBR06 | NBR06 | CONFIRMED [positive scenarios rejected: PBR04] |
| BR-9 | KobBidKron | threshold boundary: rem >= REFUND_MIN - 1, so rem = REFUND_MIN - 1 may be split off a delivery that keeps only deliveryCarrier | NBR07 | NBR07 | NBR07 | CONFIRMED [positive scenarios rejected: PBR05 PBR08] |

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
- TA-cltv: NTA04n4
- TA-ivpos: NTA04n1
- TU-cltv: NTU04n4 NTT04n4
- TU-ivpos: NTU04n1 NTT04n1
- TI-push08: NTI11
- TI-cltv: NTI04n4 NTJ04n4
- TI-ivpos: NTI04n1 NTJ04n1
- AU-KA-30: KA3 KA-IOC KA13d
- AU-KB-17: KB3 KB4
- AU-KB-25: NBR07
- AU-KCA-3: NTA02d NTU02d
- AU-KCA-4: NTA02c NTU02c
- AU-KCA-32: KC3
- AU-KIB-18x: NI2b NI6 NI8
- AU-KIB-24: KI3
- BR-2: NBR06
- BR-3x: NBR01 NBR02 NBR04 NBR06 NBR09 NBR10 NBR11 NBR12
- BR-4: NBR06
- BR-7: NBR06

## kron-buy

KRON adapter (v3, buy-side contracts); test binary `kob_kron_v2_buy_tests`; contracts `contracts/orders/kron`. 213/213 confirmed, 8/8 defence-in-depth held (expected NOT to flip).
Baseline (no mutation): kron_v2_buy_lifecycle clean, kron_v2_buy_touch_cond_arm clean, kron_v2_buy_touch_cond_trail clean, kron_v2_buy_trail_cap_attack clean, kron_v2_buy_negative clean, kron_v2_buy_v3_min_fill clean, kron_v2_buy_v3_rounding clean, kron_v2_buy_v3_merge_push clean, kron_v2_buy_lifecycle_dust_custody clean, kron_v2_buy_repeat_ifd_attacks clean, kron_v2_buy_touch_ifd_arm clean, kron_v2_buy_v3_tip clean, kron_v2_buy_touch_cond_settle clean, kron_empty_repeating_ifda_refund_is_refused clean, kron_v2_buy_audit_cond_bid clean, kron_v2_buy_touch_ifd_fill clean, kron_v2_buy_bid_refund clean, kron_v2_buy_audit_ifd_ask clean.

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
| TB-cltv | KobCondBidKron | touchBid (settle, buy-stop leg): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTB01, NTB03, NTB04 | NTB01 NTB03 NTB04 NTB04n4(F) | NTB01 NTB03 NTB04 NTB04n4(F) | CONFIRMED |
| TB-interval | KobCondBidKron | touchBid (settle, buy-stop leg): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTB04 | NTB04 | NTB04 | CONFIRMED |
| TB-ivpos | KobCondBidKron | touchBid (settle, buy-stop leg): the evidence interval is not negative (a negative one cancels the UTXO age, so a quote of the same transaction arms the stop; audit 2026-10-29 F1) | NTB04n2, NTB04n3 | NTB04n1(F) NTB04n2 NTB04n3 | NTB04n1(F) NTB04n2 NTB04n3 | CONFIRMED |
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
| TV-cltv | KobCondBidKron | touch (update: arm on a bid, trail on an ask): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTV01, NTV03, NTV04, NTW01, NTW02, NTW03, NTW04 | NTV01 NTV03 NTV04 NTV04n4(F) NTW01 NTW02 NTW03 NTW04 NTW04n4(F) | NTV01 NTV03 NTV04 NTV04n4(F) NTW01 NTW02 NTW03 NTW04 NTW04n4(F) | CONFIRMED |
| TV-interval | KobCondBidKron | touch (update: arm on a bid, trail on an ask): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTV04, NTW04 | NTV04 NTW04 | NTV04 NTW04 | CONFIRMED |
| TV-ivpos | KobCondBidKron | touch (update: arm on a bid, trail on an ask): the evidence interval is not negative (a negative one cancels the UTXO age, so a quote of the same transaction arms the stop; audit 2026-10-29 F1) | NTV04n2, NTV04n3, NTW04n2, NTW04n3 | NTV04n1(F) NTV04n2 NTV04n3 NTW04n1(F) NTW04n2 NTW04n3 | NTV04n1(F) NTV04n2 NTV04n3 NTW04n1(F) NTW04n2 NTW04n3 | CONFIRMED |
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
| TK-cltv | KobIfdAskKron | touchAsk (sell-stop entry): exposure: tx.daa >= exposed + minRestDaa (CLTV) | NTK01, NTK02, NTK03, NTK04, NTL01, NTL02, NTL03, NTL04 | NTK01 NTK02 NTK03 NTK04 NTK04n4(F) NTL01 NTL02 NTL03 NTL04 NTL04n4(F) | NTK01 NTK02 NTK03 NTK04 NTK04n4(F) NTL01 NTL02 NTL03 NTL04 NTL04n4(F) | CONFIRMED |
| TK-interval | KobIfdAskKron | touchAsk (sell-stop entry): exposure counts the TWAP / DCA interval (UTXO DAA + interval) | NTK04, NTL04 | NTK04 NTL04 | NTK04 NTL04 | CONFIRMED |
| TK-ivpos | KobIfdAskKron | touchAsk (sell-stop entry): the evidence interval is not negative (a negative one cancels the UTXO age, so a quote of the same transaction arms the stop; audit 2026-10-29 F1) | NTK04n2, NTK04n3, NTL04n2, NTL04n3 | NTK04n1(F) NTK04n2 NTK04n3 NTL04n1(F) NTL04n2 NTL04n3 | NTK04n1(F) NTK04n2 NTK04n3 NTL04n1(F) NTL04n2 NTL04n3 | CONFIRMED |
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
| AU-KCB-1 | KobCondBidKron | tplState: the evidence UTXO is the P2SH of the redeem script it pushes (both evidence readers: touchBid, touch) | NTB18, NTV18, NTW18 | NTB18 NTV18 NTW02d(F) NTW18 | NTB18 NTV18 NTW02d(F) NTW18 | CONFIRMED |
| AU-KCB-2 | KobCondBidKron | tplState: the evidence's template hash (a real order's state under another template) | NTB17, NTV17, NTW17 | NTB17 NTV17 NTW02c(F) NTW17 | NTB17 NTV17 NTW02c(F) NTW17 | CONFIRMED |
| AU-KCB-S0 | KobCondBidKron | noStrays scan slot 0: a stray of the conditional buy at token slot 0 rides to the taker | NAU-KCBS0 | NAU-KCBS0 | NAU-KCBS0 | CONFIRMED |
| AU-KCB-S1 | KobCondBidKron | noStrays scan slot 1: a stray of the conditional buy at token slot 1 rides to the taker | NAU-KCBS1 | NAU-KCBS1 | NAU-KCBS1 | CONFIRMED |
| AU-KCB-S2 | KobCondBidKron | noStrays scan slot 2: a stray of the conditional buy at token slot 2 rides to the taker | NAU-KCBS2 | NAU-KCBS2 | NAU-KCBS2 | CONFIRMED |
| AU-KCB-S3 | KobCondBidKron | noStrays scan slot 3: a stray of the conditional buy at token slot 3 rides to the taker | NAU-KCBS3 | NAU-KCBS3 | NAU-KCBS3 | CONFIRMED |
| AU-KCB-3 | KobCondBidKron | settle: the order is the only input of its covenant id (a sibling UTXO of the id beside the fill) | NAU-KCB01 | NAU-KCB01 | NAU-KCB01 | CONFIRMED |
| AU-KCB-4 | KobCondBidKron | settle: no fill before activeFrom | NAU-KCB02 | NAU-KCB02 | NAU-KCB02 | CONFIRMED |
| AU-KCB-5 | KobCondBidKron | settle n > 0: a zero fill delivers a zero-amount token output the KRON program refuses (amounts 1..1e9); on the re-arm path (no delivery) the entry's merge refuses m = 0 (NAU-KIA32 stays refused at the entry); AU-KCB-5x removes both | stay rejected: NAU-KIA32 | NAU-KIA33(F) | NAU-KIA33(F) | held (defence in depth) |
| AU-KCB-5x | KobCondBidKron | the exit's n > 0 and the entry's m > 0 removed together: a booked exit (minFill 0) settles n = 0 and its entry merges m = 0 (both continue unchanged: a no-op merge resetting both UTXO ages) | NAU-KIA32, NAU-KIA33 | NAU-KIA32 NAU-KIA33 | NAU-KIA32 NAU-KIA33 | CONFIRMED |
| AU-KCB-6 | KobCondBidKron | settle: n <= amountLeft (a fill of 4001 from an order of 4000 terminates it) | NB14 | NB14 | NB14 | CONFIRMED |
| AU-KCB-7 | KobCondBidKron | settle: scale > 0 (hostile field: at a negative scale the spend is negative) | NAU-KCB03 | NAU-KCB03 | NAU-KCB03 | CONFIRMED |
| AU-KCB-8 | KobCondBidKron | settle: tip >= 0 (hostile field: the order would pay below its leg price) | NAU-KCB04 | NAU-KCB04 | NAU-KCB04 | CONFIRMED |
| AU-KCB-9 | KobCondBidKron | settle: deliveryCarrier >= 0 (hostile field: the delivery carries nothing) | NAU-KCB05 | NAU-KCB05 | NAU-KCB05 | CONFIRMED |
| AU-KCB-10 | KobCondBidKron | limit leg only when the order has one (tpPrice > 0; a stop-only order would buy at the tip alone) | NB6 | NB6 | NB6 | CONFIRMED |
| AU-KCB-11 | KobCondBidKron | leg is 0 or 1 (leg 2 is no leg) | NAU-KCB06 | NAU-KCB06 | NAU-KCB06 | CONFIRMED |
| AU-KCB-12 | KobCondBidKron | stop leg only when the order has one (stopPrice > 0; a limit-only order would buy at price 0 after any bid fill) | NAU-KCB07 | NAU-KCB07 | NAU-KCB07 | CONFIRMED |
| AU-KCB-13 | KobCondBidKron | slipBps >= 0 (hostile field: a negative band buys below the stop) | NAU-KCB08 | NAU-KCB08 | NAU-KCB08 | CONFIRMED |
| AU-KCB-14 | KobCondBidKron | slipBps <= 10000 (band at most 100%) | NB26 | NB26 | NB26 | CONFIRMED |
| AU-KCB-15 | KobCondBidKron | stop auction: tx.daa >= t (CLTV proves the auction time; a later t is a higher buy price) | NAU-KCB09 | NAU-KCB09 | NAU-KCB09 | CONFIRMED |
| AU-KCB-16 | KobCondBidKron | stopPrice <= MAX_STOP (the band product fits; with a zero band an order above it would fill) | NAU-KCB10 | NAU-KCB10 | NAU-KCB10 | CONFIRMED |
| AU-KCB-17 | KobCondBidKron | re-arm: the maker's profit goes to P2PK(maker) at output i | NAU-KCB11 | NAU-KCB11 | NAU-KCB11 | CONFIRMED |
| AU-KCB-18 | KobCondBidKron | the delivery template is read from a token input of this token (a same-program source of another covenant gains nothing: the bytes are hash-pinned; the check pins the source) | NAU-KCB12 | NAU-KCB12 | NAU-KCB12 | CONFIRMED |
| AU-KCB-19 | KobCondBidKron | the delivery template's hash is the token's (a planted look-alike would give the maker an anyone-can-spend fake) | NAU-KCB13 | NAU-KCB13 | NAU-KCB13 | CONFIRMED |
| AU-KCB-20 | KobCondBidKron | (KB-A2 / KB-A5 / KB-A6 flip at this input only: the KRON program refuses their delivery state itself) positional delivery: output i is the holder of exactly n base units owned by the maker (id_type 3, not a minter) | NB18, KB-A1, KB-A3, KB-A4 | NB18 KB-A1 KB-A2(F) KB-A3 KB-A4 KB-A5(F) KB-A6(F) | NB18 KB-A1 KB-A2(F) KB-A3 KB-A4 KB-A5(F) KB-A6(F) | CONFIRMED |
| AU-KCB-21 | KobCondBidKron | the delivery is bound to the token covenant (an unbound copy of the holder script delivers nothing) | NAU-KCB14 | NAU-KCB14 | NAU-KCB14 | CONFIRMED |
| AU-KCB-22 | KobCondBidKron | continuation: exactly one output bound to the order's id | NAU-KCB15 | NAU-KCB15 | NAU-KCB15 | CONFIRMED |
| AU-KCB-23 | KobCondBidKron | continuation: the delivery keeps the delivery carrier | NB2 | NB2 | NB2 | CONFIRMED |
| AU-KCB-24 | KobCondBidKron | termination: no output bound to the order's id | NAU-KCB16, NBR15 | NAU-KCB16 NBR15 | NAU-KCB16 NBR15 | CONFIRMED |
| AU-KCB-25 | KobCondBidKron | termination without a refund (rem < REFUND_MIN): everything not spent rides on the delivery | NAU-KCB17, NBR08 | NAU-KCB17 NBR07 NBR08 | NAU-KCB17 NBR07 NBR08 | CONFIRMED |
| AU-KCB-26 | KobCondBidKron | update: the order is the only input of its covenant id | NAU-KCB18 | NAU-KCB18 | NAU-KCB18 | CONFIRMED |
| AU-KCB-27 | KobCondBidKron | update: only an order with a stop leg arms (a limit-only order would pay keeperTip for nothing) | NAU-KCB19 | NAU-KCB19 | NAU-KCB19 | CONFIRMED |
| AU-KCB-28 | KobCondBidKron | update: keeperTip >= 0 (hostile field) | NAU-KCB20 | NAU-KCB20 | NAU-KCB20 | CONFIRMED |
| AU-KCB-29 | KobCondBidKron | update: no arm / trail before activeFrom | NAU-KCB21 | NAU-KCB21 | NAU-KCB21 | CONFIRMED |
| AU-KCB-30 | KobCondBidKron | update: exactly one output bound to the order's id | NAU-KCB22 | NAU-KCB22 | NAU-KCB22 | CONFIRMED |
| AU-KCB-31 | KobCondBidKron | update: the continuation keeps floorOut (the budget, and at most keeperTip to the keeper) | NB25, NB30, NB30b, NB30c | NB25 NB30b NB30 NB30c | NB25 NB30b NB30 NB30c | CONFIRMED |
| AU-KCB-32 | KobCondBidKron | cancel: SIGHASH_ALL only | X-NC1 | X-NC1 | X-NC1 | CONFIRMED |
| AU-KCB-33 | KobCondBidKron | cancel: the maker's signature | X-NC2 | X-NC2 | X-NC2 | CONFIRMED |
| AU-KCB-34 | KobCondBidKron | refund only once due: min(expiryDaa, 90 days idle) | X-NC3, X-NC4 | X-NC3 X-NC4 | X-NC3 X-NC4 | CONFIRMED |
| AU-KCB-35 | KobCondBidKron | refund: the order is the only input of its covenant id | NAU-KCB23 | NAU-KCB23 | NAU-KCB23 | CONFIRMED |
| AU-KCB-36 | KobCondBidKron | refund: no output bound to the order's id (a keeper's UTXO of the id would own every later stray) | NAU-KCB24 | NAU-KCB24 | NAU-KCB24 | CONFIRMED |
| AU-KCB-37 | KobCondBidKron | refund: output i pays P2PK(maker) | X-NC7 | X-NC7 | X-NC7 | CONFIRMED |
| AU-KCB-38 | KobCondBidKron | refund: the maker gets the escrow minus at most refundTip | X-NC6 | X-NC6 | X-NC6 | CONFIRMED |
| AU-KCB-39 | KobCondBidKron | update: scale > 0. DEFENCE IN DEPTH: the evidence must quote at the same (negative) scale, and the evidence KobAskKron refuses its own negative scale (NAU-KCB25, judged at the evidence input); AU-KCB-39x removes both | stay rejected: NAU-KCB25 |  |  | held (defence in depth) |
| AU-KCB-39x | KobCondBidKron | update scale > 0 and the evidence KobAskKron's scale > 0 removed together: at scale -1000 the keeper takes keeperTip out of the buying budget | NAU-KCB25 | NAU-KCB25 | NAU-KCB25 | CONFIRMED |
| AU-KIA-1 | KobIfdAskKron | tplRs: the custody input is the P2SH of the holder script it pushes (a planted UTXO of the token covenant id) | NAU-KIA01 | NAU-KIA01 | NAU-KIA01 | CONFIRMED |
| AU-KIA-2 | KobIfdAskKron | (KI-A9, the custody under the other pinned KRON program, stays refused by another custody check; held) tplRs: the custody's template is the token's (a planted P2SH of a look-alike; the other pinned KRON program) | NAU-KIA02; stay rejected: KI-A9 | NAU-KIA02 | NAU-KIA02 | CONFIRMED |
| AU-KIA-3 | KobIfdAskKron | tplState: the evidence UTXO is the P2SH of the redeem script it pushes | NTK18, NTL18 | NTK02d(F) NTK18 NTL02d(F) NTL18 | NTK02d(F) NTK18 NTL02d(F) NTL18 | CONFIRMED |
| AU-KIA-4 | KobIfdAskKron | tplState: the evidence's template hash | NTK17, NTL17 | NTK02c(F) NTK17 NTL02c(F) NTL17 | NTK02c(F) NTK17 NTL02c(F) NTL17 | CONFIRMED |
| AU-KIA-S0 | KobIfdAskKron | noStrays scan slot 0: a stray of the entry at token slot 0 rides to the taker | NAU-KIAS0 | NAU-KIAS0 | NAU-KIAS0 | CONFIRMED |
| AU-KIA-S1 | KobIfdAskKron | noStrays scan slot 1: a stray of the entry at token slot 1 rides to the taker | NAU-KIAS1 | NAU-KIAS1 | NAU-KIAS1 | CONFIRMED |
| AU-KIA-S2 | KobIfdAskKron | noStrays scan slot 2: a stray of the entry at token slot 2 rides to the taker | NAU-KIAS2 | NAU-KIAS2 | NAU-KIAS2 | CONFIRMED |
| AU-KIA-S3 | KobIfdAskKron | noStrays scan slot 3: a stray of the entry at token slot 3 rides to the taker | NAU-KIAS3 | NAU-KIAS3 | NAU-KIAS3 | CONFIRMED |
| AU-KIA-5 | KobIfdAskKron | settle: the entry is the only input of its covenant id (a sibling UTXO of the id beside the fill) | NAU-KIA03 | NAU-KIA03 | NAU-KIA03 | CONFIRMED |
| AU-KIA-6 | KobIfdAskKron | the custody carries the token covenant id (not a UTXO of another token of the same program owned by the entry) | NAU-KIA04 | NAU-KIA04 | NAU-KIA04 | CONFIRMED |
| AU-KIA-7 | KobIfdAskKron | the custody is owned by this entry's covenant id (not tokens of another id: the entry's real custody would be orphaned) | NAU-KIA05 | NAU-KIA05 | NAU-KIA05 | CONFIRMED |
| AU-KIA-8 | KobIfdAskKron | the custody has id_type 2 (covenant id): owner bytes equal to the id under id_type 3 are not the custody | NAU-KIA06 | NAU-KIA06 | NAU-KIA06 | CONFIRMED |
| AU-KIA-9 | KobIfdAskKron | the custody is not a minter (is_minter 0) | NAU-KIA07 | NAU-KIA07 | NAU-KIA07 | CONFIRMED |
| AU-KIA-10 | KobIfdAskKron | refund only once due: min(expiryDaa, 90 days idle) | KI-R6, KI-R7 | KI-R6 KI-R7 | KI-R6 KI-R7 | CONFIRMED |
| AU-KIA-11 | KobIfdAskKron | refund: the maker gets the carriers minus at most refundTip | KI-R8 | KI-R8 | KI-R8 | CONFIRMED |
| AU-KIA-12 | KobIfdAskKron | merge: scale > 0 (the merge needs the exit at k to quote at this scale, and KobCondBidKron.settle refuses scale <= 0: NAU-KIA34 stays refused at the exit; AU-KIA-12x removes both) | stay rejected: NAU-KIA34 |  |  | held (defence in depth) |
| AU-KIA-12x | KobIfdAskKron | the entry's merge scale > 0 and the exit's scale > 0 removed together: a merge at scale -1000 leaves the entry 1.5 KAS below its value | NAU-KIA34 | NAU-KIA34 | NAU-KIA34 | CONFIRMED |
| AU-KIA-13 | KobIfdAskKron | merge: m > 0 (the exit at k must settle n = m, and KobCondBidKron.settle refuses n = 0: NAU-KIA33 stays refused at the exit; AU-KCB-5x removes both) | stay rejected: NAU-KIA33 | NAU-KIA32(F) | NAU-KIA32(F) | held (defence in depth) |
| AU-KIA-14 | KobIfdAskKron | merge: the exit terms [198..253) (slipBps .. minRestDaa) are the committed exitState | NAU-KIA08 | NAU-KIA08 | NAU-KIA08 | CONFIRMED |
| AU-KIA-15 | KobIfdAskKron | merge: the exit's armed push opcode (state [261]) is the committed one (a re-encoded exit is another script) | NAU-KIA09 | NAU-KIA09 | NAU-KIA09 | CONFIRMED |
| AU-KIA-16 | KobIfdAskKron | merge: the exit terms [270..288) (bandDaa, keeperTip) are the committed exitState | NAU-KIA10 | NAU-KIA10 | NAU-KIA10 | CONFIRMED |
| AU-KIA-17 | KobIfdAskKron | fill: not before activeFrom | NAU-KIA11 | NAU-KIA11 | NAU-KIA11 | CONFIRMED |
| AU-KIA-18 | KobIfdAskKron | fill: scale > 0 (hostile field: at a negative scale the proceeds are negative) | NAU-KIA12 | NAU-KIA12 | NAU-KIA12 | CONFIRMED |
| AU-KIA-19 | KobIfdAskKron | fill: tip >= 0 (hostile field) | NI12 | NI12 | NI12 | CONFIRMED |
| AU-KIA-20 | KobIfdAskKron | fill: prefund >= 0 (hostile field: the exit would hold less than the proceeds) | NAU-KIA13 | NAU-KIA13 | NAU-KIA13 | CONFIRMED |
| AU-KIA-21 | KobIfdAskKron | fill: exitCarrier >= 0 (hostile field) | NAU-KIA14 | NAU-KIA14 | NAU-KIA14 | CONFIRMED |
| AU-KIA-22 | KobIfdAskKron | fill: n <= amountLeft (the exit would record more than the entry sold) | NAU-KIA15 | NAU-KIA15 | NAU-KIA15 | CONFIRMED |
| AU-KIA-23 | KobIfdAskKron | entry auction: tx.daa >= t (CLTV proves the time; a later t is a lower sell price) | NAU-KIA16 | NAU-KIA16 | NAU-KIA16 | CONFIRMED |
| AU-KIA-24 | KobIfdAskKron | entry auction: t >= origin (a time before the origin is refused; it could only raise the price) | NAU-KIA17 | NAU-KIA17 | NAU-KIA17 | CONFIRMED |
| AU-KIA-25 | KobIfdAskKron | repeat: while a minimum fill of re-arms is left, a fill takes at most that many (and is booked) | NRPB6b | NRPB6b | NRPB6b | CONFIRMED |
| AU-KIA-26 | KobIfdAskKron | the exit's template is KobCondBidKron (COND_BID_TPL; cPre / cSuf of an attacker template build an anyone-can-spend exit) | NAU-KIA18 | NAU-KIA18 | NAU-KIA18 | CONFIRMED |
| AU-KIA-27 | KobIfdAskKron | rest: the remainder custody keeps its carrier | KI-A10 | KI-A10 | KI-A10 | CONFIRMED |
| AU-KIA-28 | KobIfdAskKron | sold out: the last exit gets the proceeds plus both carriers | NI7 | NI7 | NI7 | CONFIRMED |
| AU-KIA-29 | KobIfdAskKron | continuation: exactly one output bound to the entry's id | NAU-KIA19 | NAU-KIA19 | NAU-KIA19 | CONFIRMED |
| AU-KIA-30 | KobIfdAskKron | continuation: the entry keeps its floor (fill, sold-out repeating, merge) | NI8, NRPB8, NRPB15, NRPB16 | NI8 NRPB8 NRPB15 NRPB16 NRPB31 | NI8 NRPB8 NRPB15 NRPB16 NRPB31 | CONFIRMED |
| AU-KIA-31 | KobIfdAskKron | termination: no output bound to the entry's id | NAU-KIA20 | NAU-KIA20 | NAU-KIA20 | CONFIRMED |
| AU-KIA-32 | KobIfdAskKron | close: the entry is the only input of its covenant id | NAU-KIA21 | NAU-KIA21 | NAU-KIA21 | CONFIRMED |
| AU-KIA-33 | KobIfdAskKron | close: no output bound to the entry's id | NAU-KIA22 | NAU-KIA22 | NAU-KIA22 | CONFIRMED |
| AU-KIA-34 | KobIfdAskKron | close: output i pays P2PK(maker) | NAU-KIA23 | NAU-KIA23 | NAU-KIA23 | CONFIRMED |
| AU-KIA-35 | KobIfdAskKron | close: the maker gets the carrier minus at most refundTip | NAU-KIA24 | NAU-KIA24 | NAU-KIA24 | CONFIRMED |
| AU-KIA-36 | KobIfdAskKron | update: the entry is the only input of its covenant id | NAU-KIA25 | NAU-KIA25 | NAU-KIA25 | CONFIRMED |
| AU-KIA-37 | KobIfdAskKron | update: only a stop entry arms (entryStop > 0) | NAU-KIA26 | NAU-KIA26 | NAU-KIA26 | CONFIRMED |
| AU-KIA-38 | KobIfdAskKron | update: only while unarmed (an armed entry would pay keeperTip again, an auction would restart) | NAU-KIA27 | NAU-KIA27 | NAU-KIA27 | CONFIRMED |
| AU-KIA-39 | KobIfdAskKron | update: keeperTip >= 0 (hostile field) | NAU-KIA28 | NAU-KIA28 | NAU-KIA28 | CONFIRMED |
| AU-KIA-40 | KobIfdAskKron | update: no arm before activeFrom | NAU-KIA29 | NAU-KIA29 | NAU-KIA29 | CONFIRMED |
| AU-KIA-41 | KobIfdAskKron | update: exactly one output bound to the entry's id | NAU-KIA30 | NAU-KIA30 | NAU-KIA30 | CONFIRMED |
| AU-KIA-42 | KobIfdAskKron | update: the continuation is this script with armed = 1 (no other field changed) | NAU-KIA31 | NAU-KIA31 | NAU-KIA31 | CONFIRMED |
| AU-KIA-43 | KobIfdAskKron | cancel: SIGHASH_ALL only | KI-C1 | KI-C1 | KI-C1 | CONFIRMED |
| AU-KIA-44 | KobIfdAskKron | cancel: the maker's signature | KI-C2 | KI-C2 | KI-C2 | CONFIRMED |
| AU-KIA-45 | KobIfdAskKron | booking: n < MERGE_K (2^53), so the merge argument -(k * 2^53 + n) does not overflow into k; the KRON program refuses a custody above its amount cap on its own input, so only the entry flips (as V3-C-mergek in kron-sell) | NAU-KIA35 | NAU-KIA35(F) | NAU-KIA35(F) | CONFIRMED |
| BR-1 | KobCondBidKron | refundTo aliasing guard: the input at the refund slot is a token input or a plain key input, not another order (a KobAskKron of the same maker whose payout doubles as the refund) | NBR14 | NBR14 | NBR14 | CONFIRMED |
| BR-2 | KobCondBidKron | refundTo: the output at M - 1 - self pays P2PK(maker) (another key, a non-P2PK script, no refund output, one output for two orders, swapped refunds, a slot off by one) | NBR01, NBR02, NBR04, NBR09, NBR10, NBR11, NBR12 | NBR01 NBR02 NBR04 NBR06 NBR09 NBR10 NBR11 NBR12 | NBR01 NBR02 NBR04 NBR06 NBR09 NBR10 NBR11 NBR12 | CONFIRMED |
| BR-3 | KobCondBidKron | refundTo: the refund carries at least rem (1 sompi short is refused; a spare output after the refund is also refused by the P2PK check) | NBR03; stay rejected: NBR13 | NBR03 | NBR03 | CONFIRMED |
| BR-3x | KobCondBidKron | refundTo: P2PK(maker) and value >= rem removed together: a slot one past the refund (a spare output) is accepted | NBR03, NBR13 | NBR01 NBR02 NBR03 NBR04 NBR06 NBR09 NBR10 NBR11 NBR12 NBR13 | NBR01 NBR02 NBR03 NBR04 NBR06 NBR09 NBR10 NBR11 NBR12 NBR13 | CONFIRMED |
| BR-4 | KobCondBidKron | terminal fill: refundTo is called at all (every refund-layout attack that keeps the delivery at deliveryCarrier or more is accepted) | NBR01, NBR02, NBR03, NBR04, NBR09, NBR10, NBR11, NBR12, NBR13, NBR14 | NBR01 NBR02 NBR03 NBR04 NBR06 NBR09 NBR10 NBR11 NBR12 NBR13 NBR14 | NBR01 NBR02 NBR03 NBR04 NBR06 NBR09 NBR10 NBR11 NBR12 NBR13 NBR14 | CONFIRMED |
| BR-5 | KobCondBidKron | terminal fill with a refund: the delivery keeps deliveryCarrier (1 sompi below it, refund 1 sompi above rem) | NBR05 | NBR05 | NBR05 | CONFIRMED |
| BR-7 | KobCondBidKron | the refund branch is entered only by the condition: if (false) always takes the delivery-only branch, so a missing refund at rem 6 KAS is accepted | NBR04 | NBR04 NBR06 | NBR04 NBR06 | CONFIRMED [positive scenarios rejected: PBR01 PBR02 PBR04 PBR05 PBR06 PBR07 PBR08 PBR09 PBR10 PBR11] |
| BR-8 | KobCondBidKron | threshold boundary: rem >= REFUND_MIN becomes rem > REFUND_MIN, so rem exactly REFUND_MIN may stay on the delivery | NBR06 | NBR06 | NBR06 | CONFIRMED [positive scenarios rejected: PBR02] |
| BR-9 | KobCondBidKron | threshold boundary: rem >= REFUND_MIN - 1, so rem = REFUND_MIN - 1 may be split off a delivery that keeps only deliveryCarrier | NBR07 | NBR07 | NBR07 | CONFIRMED [positive scenarios rejected: PBR03 PBR07] |

Collateral flips (scenarios accepted besides the expected ones):

- IA11: NI18b
- IA12: NRPB35
- IA19: NRPB31
- IA20: NRPB31
- IA24: NRPB6
- FXL3d: NRPB20
- TB-push08: NTB11
- TB-cltv: NTB04n4
- TB-ivpos: NTB04n1
- TV-cltv: NTV04n4 NTW04n4
- TV-ivpos: NTV04n1 NTW04n1
- TK-cltv: NTK04n4 NTL04n4
- TK-ivpos: NTK04n1 NTL04n1
- AU-KCB-1: NTW02d
- AU-KCB-2: NTW02c
- AU-KCB-5: NAU-KIA33
- AU-KCB-20: KB-A2 KB-A5 KB-A6
- AU-KCB-25: NBR07
- AU-KIA-3: NTK02d NTL02d
- AU-KIA-4: NTK02c NTL02c
- AU-KIA-13: NAU-KIA32
- AU-KIA-30: NRPB31
- BR-2: NBR06
- BR-3x: NBR01 NBR02 NBR04 NBR06 NBR09 NBR10 NBR11 NBR12
- BR-4: NBR06
- BR-7: NBR06

## pair

pair orders: KobPair; test binary `kob_pair_tests`; contracts `contracts/orders`. 101/101 confirmed, 3/3 defence-in-depth held (expected NOT to flip).
Baseline (no mutation): pair_aliasing clean, pair_custody clean, pair_params clean, pair_settlement clean, pair_strays clean, pair_lifecycle clean, pair_fake_quotes clean, pair_delivery_modes clean, pair_carriers clean, kob_cond_pair_tests::cond_pair_evidence_fake_quotes clean, pair_second_topup clean, pair_outputs clean.

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
| C10k | KobPair | KCC-20 custody of the extension commitment sExt (units of S's covenant id with another commitment are another token; audit 2026-10-13: before, only kob_pair_custody_ext_tests asserted it) | NP28sk | NP28sk | CONFIRMED |
| C6k | KobPair | sFamily is 1 or 2 (hostile field read as KCC-20) | NP22sk | NP22sk | CONFIRMED |
| C7 | KobPair | a positive custody: a zero custody leaves output i of a refund unpinned (a KRON program also refuses the empty token output) | NP27a, NP27b | NP27a NP27b(F) NP27a(F) NP27b | CONFIRMED |
| C9s | KobPair | sScale > 0 (hostile field: an ask at a negative scale of A sells for one unit of B) | NP64a | NP64a | CONFIRMED |
| C9t | KobPair | tScale > 0 (hostile field: a bid at a negative scale of A pays a negative quote) | NP64b | NP64b | CONFIRMED |
| C8 | KobPair | exact custody: held == custody (an extra unit to the matcher) | NP06, NP07 | NP06 NP07 | CONFIRMED |
| G1 | KobPair | no S input owned by this id but the custody (rest, IOC, close, refund) | NP30a0, NP30a1, NP30a2, NP30b0, NP30b1, NP30b2, NP32a, NP32b | NP30a0 NP30a1 NP30a2 NP32a NP30b0 NP30b1 NP30b2 NP32b | CONFIRMED |
| G2 | KobPair | no T input owned by this id in a fill (rest, IOC, close) but the named bought-custody (mode 1: NM10) | NP31a0, NP31a1, NP31a2, NP31b0, NP31b1, NP31b2, atkNM10, btkNM10 | NP31a0 NP31a1 NP31a2 NP31b0 NP31b1 NP31b2 NP34t0 NP34t1 NP34t2 NP34t3 NP34t4 NP34t5 NP34t6 NP34t7 NP35t(F) NP35t atkNM10 btkNM10 | CONFIRMED |
| GS0 | KobPair | noStrays scan slot 0: a stray of the order at the input of slot 0 of its token | NP34t0 | NP34t0 | CONFIRMED |
| GS1 | KobPair | noStrays scan slot 1: a stray of the order at the input of slot 1 of its token | NP34t1 | NP30a0 NP31a0 NP30a1 NP31a1 NP30a2 NP31a2 NP32a NP30b0 NP31b0 NP30b1 NP31b1 NP30b2 NP31b2 NP32b NP34t1 | CONFIRMED |
| GS2 | KobPair | noStrays scan slot 2: a stray of the order at the input of slot 2 of its token | NP34t2 | NP34t2 | CONFIRMED |
| GS3 | KobPair | noStrays scan slot 3: a stray of the order at the input of slot 3 of its token | NP34t3 | NP34t3 | CONFIRMED |
| GS4 | KobPair | noStrays scan slot 4: a stray of the order at the input of slot 4 of its token | NP34t4 | NP34t4 | CONFIRMED |
| GS5 | KobPair | noStrays scan slot 5: a stray of the order at the input of slot 5 of its token | NP34t5 | NP34t5 | CONFIRMED |
| GS6 | KobPair | noStrays scan slot 6: a stray of the order at the input of slot 6 of its token | NP34t6 | NP34t6 | CONFIRMED |
| GS7 | KobPair | noStrays scan slot 7: a stray of the order at the input of slot 7 of its token | NP34t7 | NP34t7 | CONFIRMED |
| GB | KobPair | noStrays bound: at most MAX_TOK_IN = 8 inputs of the token (a 9th is not scanned; the KCC-20 program refuses it on its own input) | NP35t | NP35t(F) NP35t | CONFIRMED |
| G3 | KobPair | a refund carries no T input (T strays stay put); a mode-1 fill counts exactly its T inputs (NM44) | NP33a, NP33b, atkNM44, btkNM44 | NP33a NP33b atkNM44 btkNM44 | CONFIRMED |
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
| T2k | KobPair | KCC-20 delivery owned by a key (scheme 0x00; the T delivery state is now dMid, no longer tokenState, so both are mutated) | NP91tk | NP91tk | CONFIRMED |
| T2r | KobPair | KRON delivery of id_type 3 (address; the KRON T delivery state is now dSt, no longer tokenState, so both are mutated) | NP91tr | NP91tr | CONFIRMED |
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
| K5 | KobPair | the delivery carries deliveryCarrier (a new delivery: haveKas + the carrier, NM17 / NM24 / NM75) | NP114a, NP114b, atkNM17, btkNM17, atkNM24, btkNM24, atkNM75, btkNM75 | NP114a NP114b atkNM17 atkNM24 atkNM75(F) btkNM17 btkNM24 btkNM75(F) | CONFIRMED |
| K6 | KobPair | FOK: all or nothing | NP53 | NP53 | CONFIRMED |
| K7 | KobPair | a terminating fill leaves no output bound to the order id | NP44a, NP44b | NP44a NP44b | CONFIRMED |
| K8 | KobPair | the IOC return keeps its carrier | NP115a, NP115b | NP115a NP115b | CONFIRMED |
| K9 | KobPair | IOC / done: the maker gets the order value back but the tip (now one `>= keep` check after the outAmount branch, shared with K10; removed here only for outAmount > 0) | NP116a, NP116b | NP116a NP116b | CONFIRMED |
| K10 | KobPair | sold out: every carrier back to the maker but the tip (now one `>= keep` check after the outAmount branch, shared with K9; removed here only for outAmount == 0) | NP117a, NP117b | NP117a NP117b | CONFIRMED |
| X1 | KobPair | cancel: SIGHASH_ALL only | NP79a, NP79b | NP79a NP79b | CONFIRMED |
| X2 | KobPair | cancel: the maker signs | NP80a, NP80b | NP80a NP80b | CONFIRMED |
| DM-P1 | KobPair | mode 1 later fill: the bought-custody is known by its exact state (amount bought, plain owner = the order id, zero guard, the commitment) | atkNM01, btkNM01, atkNM02, btkNM02, atkNM03, btkNM03, atkNM04, btkNM04, atkNM07, btkNM07, atkNM19, btkNM19, atkNM05, btkNM05, atkNM06, btkNM06 | atkNM01 atkNM02 atkNM03 atkNM04 atkNM05(F) atkNM06(F) atkNM07 atkNM19 btkNM01 btkNM02 btkNM03 btkNM04 btkNM05(F) btkNM06(F) btkNM07 btkNM19 | CONFIRMED |
| DM-P5 | KobPair | mode 2 top-up: the receive UTXO is the P2SH of the script its state is read from | atkNM79, btkNM79 | atkNM79 btkNM79 btkNM79(F) atkNM79(F) | CONFIRMED |
| DM-P10 | KobPair | mode 2 top-up: output i is the receive UTXO's successor (DLV-07: a second order claims the receive UTXO another order tops up) | aNM20, bNM20 | aNM20 bNM20 | CONFIRMED |
| DM-P11 | KobPair | mode 2 top-up: output i is the receive UTXO's successor (DLV-07, one order: the successor at a lower output, output i a copy of its state) | atkNM85, btkNM85 | atkNM85 btkNM85 | CONFIRMED |
| DM-P9 | KobPair | mode 1 later fill: the grown custody keeps its KAS | atkNM42, btkNM42 | atkNM42 btkNM42 | CONFIRMED |
| DM-P12 | KobPair | a bought-custody is KCC-20 only: defence in depth (a KRON T held as a custody is also refused by the custody's KCC-20 state equality, a KRON state starting with its owner push, not 0x08, and in a fill by mode == 0) | stay rejected: atrNM93, btrNM93 |  | held (defence in depth) |
| DM-P13 | KobPair | mode 1 later fill: the bought-custody is the P2SH of the script its state is read from (a planted non-P2SH input of T's covenant id with the genuine custody redeem; on the public-mint and KaspaRocket programs the token program also refuses it) | atkNM08, btkNM08 | atkNM08 btkNM08 btkNM08(F) atkNM08(F) | CONFIRMED |
| DM-P14 | KobPair | mode >= 0 (hostile field: mode -1 would fill as mode 0) | atkNM51, btkNM51 | atkNM51 btkNM51 | CONFIRMED |
| DM-P15 | KobPair | mode <= 2 (hostile field: mode 3 would fill as mode 0) | atkNM50, btkNM50 | atkNM50 btkNM50 | CONFIRMED |
| DM-P16 | KobPair | a KRON T is delivered new: mode 0 only (NM90, mode 1, is not listed: its continuation keeps bought 0, which the continuation pin also refuses) (modes 1 and 2 would record a bought amount or a receive form the KRON delivery ignores) | atrNM91, btrNM91, atrNM92, btrNM92 | atrNM91 atrNM92 btrNM91 btrNM92 | CONFIRMED |
| DM-P17 | KobPair | mode 1: bought >= 0 (hostile field: bought -5 delivers a custody of n - 5, the matcher keeps 5) | atkNM67, btkNM67 | atkNM67 btkNM67 | CONFIRMED |
| DM-P18 | KobPair | modes 0 and 2: bought == 0 (a mode-0 / mode-2 order with bought > 0 and its custody spent delivers n and leaves the custody's units to the matcher, NM52b / NM53b; without the custody, NM52 / NM53, the bought-custody check refuses it too) | atkNM52b, btkNM52b, atkNM53b, btkNM53b; stay rejected: atkNM52, btkNM52, atkNM53, btkNM53 | atkNM52b atkNM53b btkNM52b btkNM53b | CONFIRMED |
| DM-P19 | KobPair | mode 2 forms 1 and 2: recvThreshold > 0 (a receive UTXO of threshold 0; a top-up into any threshold) | atkNM62, btkNM62, atkNM77, btkNM77 | atkNM62 atkNM77 btkNM62 btkNM77 | CONFIRMED |
| DM-P20 | KobPair | mode 2 forms 1 and 2: topupTip >= 0 (hostile field) | atkNM65, btkNM65 | atkNM65 btkNM65 | CONFIRMED |
| DM-P21 | KobPair | mode 2: topupTip <= deliveryCarrier (the filler of a top-up takes more than the carrier it saves, up to the order's whole KAS) | atkNM64, btkNM64, atkNM86, btkNM86, atkNM87, btkNM87 | atkNM64 atkNM86 atkNM87 btkNM64 btkNM86 btkNM87 | CONFIRMED |
| DM-P22 | KobPair | mode 2: form is 0, 1 or 2 (form 3 would fill as a top-up, NM63b; form 3 on a form-1 build, NM63, is also refused by the top-up's receive-state checks) | atkNM63b, btkNM63b; stay rejected: atkNM63, btkNM63 | atkNM63b btkNM63b | CONFIRMED |
| DM-P23 | KobPair | top-up: the receive UTXO is the maker's (owner, schemes 0x00 / 0x01, amount-threshold): a receive UTXO of another key topped up | atkNM70, btkNM70 | atkNM70 btkNM70 | CONFIRMED |
| DM-P24 | KobPair | top-up: the receive UTXO's threshold is at least recvThreshold | atkNM71, btkNM71 | atkNM71 btkNM71 | CONFIRMED |
| DM-P25 | KobPair | top-up: the receive UTXO's guard is canonical (NM72) and it carries tExt: defence in depth for the commitment, since output i is pinned with tExt and the token program keeps the successor's commitment (NM73 held) | atkNM72, btkNM72; stay rejected: atkNM73, btkNM73 | atkNM72 btkNM72 | CONFIRMED |

Collateral flips (scenarios accepted besides the expected ones):

- G2: NP34t0 NP34t1 NP34t2 NP34t3 NP34t4 NP34t5 NP34t6 NP34t7 NP35t
- GS1: NP30a0 NP31a0 NP30a1 NP31a1 NP30a2 NP31a2 NP32a NP30b0 NP31b0 NP30b1 NP31b1 NP30b2 NP31b2 NP32b
- E1c: NP73a NP73b
- O12: NP76a NP76b
- K3: NP118a NP118b

## cond-pair

pair orders: KobCondPair (excl. repeat merges); test binary `kob_cond_pair_tests`; contracts `contracts/orders`. 145/145 confirmed, 11/11 defence-in-depth held (expected NOT to flip).
Baseline (no mutation): cond_pair_lifecycle clean, cond_pair_settlement clean, cond_pair_updates clean, cond_pair_evidence_ev1 clean, cond_pair_evidence_ev0 clean, cond_pair_evidence_tk clean, audit_pair_sec_tests::aud_ext_cond_pair_refuses_evidence_of_another_extension_commitment clean, cond_pair_evidence_exposure clean, cond_pair_trailing clean, cond_pair_fills clean, cond_pair_overflow clean, cond_pair_custody clean, cond_pair_evidence_misc clean, cond_pair_carriers clean, cond_pair_delivery_modes clean, kob_ifd_pair_tests::cond_rpt_profit clean, cond_pair_second_topup clean.

| id | contract | check ablated | expected | flipped | status |
|---|---|---|---|---|---|
| S1 | KobCondPair | the order is the only input of its covenant id (a sibling UTXO of the id) | NC17a, NC17b | NC17a NC17b | CONFIRMED |
| C1 | KobCondPair | exact custody: held == custody | NC04a, NC04b | NC04a NC04b | CONFIRMED |
| C2 | KobCondPair | a refund needs a positive custody (a zero custody leaves output i unpinned) | NC82a, NC82b | NC82a NC82b(F) NC82a(F) NC82b | CONFIRMED |
| G1 | KobCondPair | no S input owned by this id but the custody (fill, refund; the update spends none) | NC06a, NC06b, NC08a, NC08b, NC76 | NC06a NC08a NC06b NC08b NC76 | CONFIRMED |
| G2 | KobCondPair | no T input owned by this id | NC07a, NC07b, NC09a, NC09b, NC77 | NC07a NC09a NC07b NC09b NC18t0 NC18t1 NC18t2 NC18t3 NC18t4 NC18t5 NC18t6 NC18t7 NC19t(F) NC19t NC77 | CONFIRMED |
| G3 | KobCondPair | an update spends no S input of the order (cust = -1 so the custody is not exempted) | NC76 | NC76 | CONFIRMED |
| GS0 | KobCondPair | noStrays scan slot 0: a stray of the order at the input of slot 0 of its token | NC18t0 | NC09a NC09b NC18t0 | CONFIRMED |
| GS1 | KobCondPair | noStrays scan slot 1: a stray of the order at the input of slot 1 of its token | NC18t1 | NC06a NC07a NC08a NC06b NC07b NC08b NC18t1 | CONFIRMED |
| GS2 | KobCondPair | noStrays scan slot 2: a stray of the order at the input of slot 2 of its token | NC18t2 | NC18t2 | CONFIRMED |
| GS3 | KobCondPair | noStrays scan slot 3: a stray of the order at the input of slot 3 of its token | NC18t3 | NC18t3 | CONFIRMED |
| GS4 | KobCondPair | noStrays scan slot 4: a stray of the order at the input of slot 4 of its token | NC18t4 | NC18t4 | CONFIRMED |
| GS5 | KobCondPair | noStrays scan slot 5: a stray of the order at the input of slot 5 of its token | NC18t5 | NC18t5 | CONFIRMED |
| GS6 | KobCondPair | noStrays scan slot 6: a stray of the order at the input of slot 6 of its token | NC18t6 | NC18t6 | CONFIRMED |
| GS7 | KobCondPair | noStrays scan slot 7: a stray of the order at the input of slot 7 of its token | NC18t7 | NC18t7 | CONFIRMED |
| GB | KobCondPair | noStrays bound: at most MAX_TOK_IN = 8 inputs of the token (a 9th is not scanned; the KCC-20 program refuses it on its own input) | NC19t | NC19t(F) NC19t | CONFIRMED |
| N1 | KobCondPair | n <= amountLeft | NC05 | NC05 | CONFIRMED |
| N2 | KobCondPair | an ask receives at least the ceil | NC01 | NC01 | CONFIRMED |
| N2r | KobCondPair | an ask's leg price is rounded up | NC01 | NC01 | CONFIRMED |
| N3 | KobCondPair | a bid pays at most the floor | NC02 | NC02 | CONFIRMED |
| N4 | KobCondPair | a bid receives exactly n | NC03 | NC03 | CONFIRMED |
| O1 | KobCondPair | the maker's token / the S output pinned (owner, amount, index) | NC11a, NC11b, NC84a, NC84b | NC11a NC11b NC84a NC84b | CONFIRMED |
| O2 | KobCondPair | the S output carries the S covenant | NC16a, NC16b | NC16a NC16b | CONFIRMED |
| O3 | KobCondPair | the maker's token output at i is pinned (delivery / profit; the pin is now split: this is the T pin, the BID exit's S profit pin under dS is a separate check) | NC13a, NC13b | NC13a NC13b | CONFIRMED |
| O4 | KobCondPair | the maker's token output carries its token covenant (split like O3: this is the T delivery's) | NC14a, NC14b | NC14a NC14b | CONFIRMED |
| K1 | KobCondPair | one continuation on a partial fill | NC10a, NC10b | NC10a NC10b | CONFIRMED |
| EVPUSH | KobCondPair | rd: the evidence sigscript starts with the 8-byte fill push (a cancel ground to read as a fill) | NC2Ca, NC2Cb, NC3Ca, NC3Cb | NC2Ca NC2Cb NC3Ca NC3Cb | CONFIRMED |
| EVN | KobCondPair | rd: the evidence is filled (n > 0) | NC20a, NC20b, NC30a, NC30b | NC20a(F) NC20b(F) NC30a(F) NC30b(F) | CONFIRMED |
| EVMT | KobCondPair | rd: n >= minTouch | NC24a, NC24b, NC34a, NC34b | NC24a(F) NC24b(F) NC34a NC34b | CONFIRMED |
| EVID | KobCondPair | rd: token and scale (mode 1: side, S, T and both scales) are the order's | NC22a, NC22b, NC23a, NC23b, NC32a, NC32b, NC33a, NC33b, NC35a, NC35b | NC22a(F) NC23a(F) NC22b(F) NC23b(F) NC32a(F) NC33a(F) NC35a(F) NC32b(F) NC33b(F) NC35b(F) | CONFIRMED |
| EVSLOPE | KobCondPair | rd: the evidence is not decaying | NC21a, NC21b, NC31a, NC31b | NC21a(F) NC21b(F) NC31a(F) NC31b(F) | CONFIRMED |
| EVREST | KobCondPair | rd: rested >= minRestDaa before the lock (every exposure term) | NC50, NC52, NC53, NC51a, NC3Ea, NC3Eb | NC50 NC52(F) NC53(F) NC51a NC3Ea NC3Eb | CONFIRMED |
| EVIV | KobCondPair | rd: the evidence interval is not negative (an A evidence / pair evidence one DAA old with interval -10^12 would arm the stop; audit 2026-10-29 F1) | NC5V, NC3Va, NC3Vb | NC5V(F) NC3Va(F) NC3Vb(F) | CONFIRMED |
| EVINT | KobCondPair | rd: the interval exposure term | NC53 | NC53(F) | CONFIRMED |
| EVACT | KobCondPair | rd: the activeFrom exposure term | NC52 | NC52(F) | CONFIRMED |
| EVTKDAA | KobCondPair | rd: the ask evidence's custody DAA exposure term | NC51a | NC51a | CONFIRMED |
| EVTK | KobCondPair | rd: the ask evidence's custody is a token input of its token | NC59a | NC59a | CONFIRMED |
| EVTKOWN | KobCondPair | rd: the ask evidence's custody is owned by the evidence ask (covenant marker) | NC57a | NC57a | CONFIRMED |
| EVTKTPL | KobCondPair | rd: the ask / pair evidence's custody is a holder of the token's program (template hash; P2SH kept): an old look-alike holder of another program refused | NC5Ha, NC3Ha, NC3Hb | NC5Ha(F) NC3Ha(F) NC3Hb(F) | CONFIRMED |
| EVTKP2SH | KobCondPair | rd: the ask / pair evidence's custody is the P2SH of the holder redeem it shows (template hash kept): an old non-P2SH input with the genuine redeem refused | NC5Sa, NC3Sa, NC3Sb | NC5Sa(F) NC3Sa(F) NC3Sb(F) | CONFIRMED |
| EVEXT | KobCondPair | rd: a KCC-20 KAS-book bid of A names A's commitment (its extensionCommitment, read with its scale) | NAUD-EXT-C0b | NAUD-EXT-C0b | CONFIRMED |
| EVCEXT | KobCondPair | rd: a KCC-20 custody of the ask / pair evidence is of its token's commitment (mode 0 the A ask; mode 1 the pair order's S) | NAUD-EXT-C0a, NAUD-EXT-C1a, NAUD-EXT-C1bB | NAUD-EXT-C0a NAUD-EXT-C1a NAUD-EXT-C1bB | CONFIRMED |
| EVTEXT | KobCondPair | rd: the pair evidence's tExt is the commitment of its T (B of a pair ASK, A of a pair BID) | NAUD-EXT-C1aB, NAUD-EXT-C1b | NAUD-EXT-C1aB NAUD-EXT-C1b | CONFIRMED |
| EVBPUSH | KobCondPair | evLeg: the B evidence sigscript starts with the 8-byte fill push (a cancel ground to read as a fill) | NC5Da, NC5Db | NC5Da NC5Db | CONFIRMED |
| EVBN | KobCondPair | evLeg: the B evidence is filled (n > 0) | NC2Na, NC2Nb | NC2Na(F) NC2Nb(F) | CONFIRMED |
| EVBMT | KobCondPair | evLeg: n_B >= ceil(minTouch * stop / scale(A)) | NC25a, NC25b | NC25a(F) NC25b(F) | CONFIRMED |
| EVBTOK | KobCondPair | evLeg: the B evidence is of token B | NC2Ta, NC2Tb | NC2Ta(F) NC2Tb(F) | CONFIRMED |
| EVBSCALE | KobCondPair | evLeg: the B evidence quotes at B's scale | NC28a, NC28b | NC28a(F) NC28b(F) | CONFIRMED |
| EVBSLOPE | KobCondPair | evLeg: the B evidence is not decaying | NC29a, NC29b | NC29a(F) NC29b(F) | CONFIRMED |
| EVBREST | KobCondPair | evLeg: rested >= minRestDaa (every exposure term) | NC2Ea, NC2Eb, NC2Ia, NC2Ib, NC2Aa, NC2Ab, NC51b | NC2Ea NC2Ia(F) NC2Aa(F) NC2Eb NC2Ib(F) NC2Ab(F) NC51b | CONFIRMED |
| EVBIV | KobCondPair | evLeg: the B evidence interval is not negative (a B evidence one DAA old with interval -10^12 would arm the stop; audit 2026-10-29 F1) | NC2Va, NC2Vb | NC2Va(F) NC2Vb(F) | CONFIRMED |
| EVBINT | KobCondPair | evLeg: the interval exposure term | NC2Ia, NC2Ib | NC2Ia(F) NC2Ib(F) | CONFIRMED |
| EVBACT | KobCondPair | evLeg: the activeFrom exposure term | NC2Aa, NC2Ab | NC2Aa(F) NC2Ab(F) | CONFIRMED |
| EVBTKDAA | KobCondPair | evLeg: the ask evidence's custody DAA exposure term | NC51b | NC51b | CONFIRMED |
| EVBTK | KobCondPair | evLeg: the ask evidence's custody is a token input of its token | NC59b | NC59b | CONFIRMED |
| EVBTKOWN | KobCondPair | evLeg: the ask evidence's custody is owned by the evidence ask | NC57b | NC57b | CONFIRMED |
| EVBTKTPL | KobCondPair | evLeg: the B ask evidence's custody is a holder of token B's program (template hash; P2SH kept): an old look-alike holder of another program refused | NC5Hb | NC5Hb(F) | CONFIRMED |
| EVBTKP2SH | KobCondPair | evLeg: the B ask evidence's custody is the P2SH of the holder redeem it shows (template hash kept): an old non-P2SH input with the genuine redeem refused | NC5Sb | NC5Sb(F) | CONFIRMED |
| EVBEXT | KobCondPair | evLeg (AUD-EXT): a KCC-20 bid of B names B's commitment | NAUD-EXT-C0aB | NAUD-EXT-C0aB | CONFIRMED |
| EVBTKEXT | KobCondPair | evLeg (AUD-EXT): the custody of a KCC-20 ask of B is of B's commitment | NAUD-EXT-C0bB | NAUD-EXT-C0bB | CONFIRMED |
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
| C10k | KobCondPair | KCC-20 custody of the extension commitment sExt (another commitment is another token; audit 2026-10-13: before, only kob_pair_custody_ext_tests asserted it) | NC107sk | NC107sk | CONFIRMED |
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
| F6 | KobCondPair | scale(A) > 0 of an ask (sScale; a negative scale makes the ceil of the quote negative) | NC114 | NC114 | CONFIRMED |
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
| K7 | KobCondPair | done with a return: the maker gets the order value but the tip (now one `>= keep` check after the outAmount branch, shared with K8; removed here only for outAmount > 0) | NC136b | NC136b | CONFIRMED |
| K8 | KobCondPair | sold out: every carrier back to the maker but the tip (now one `>= keep` check after the outAmount branch, shared with K7; removed here only for outAmount == 0) | NC137a, NC137b | NC137a NC137b | CONFIRMED |
| Q7 | KobCondPair | a rest keeps something in the custody (no continuation with an empty escrow) | NC138 | NC138 | CONFIRMED |
| EVMODE | KobCondPair | evMode is 0 or 1 | NC140 | NC140 | CONFIRMED |
| EVB0 | KobCondPair | the B quote is positive (a buy stop would arm on any B ask at 0) | NC141 | NC141(F) | CONFIRMED |
| TRGAP | KobCondPair | trailGap >= 0 (hostile field: a sell stop would trail above the rate) | NC143 | NC143 | CONFIRMED |
| TRSTEP | KobCondPair | trailStep > 0: defence in depth (a zero / negative step makes validity and maximality contradict) | stay rejected: NC144 |  | held (defence in depth) |
| TRX | KobCondPair | a buy stop's s' - gap >= 0: defence in depth (a negative x makes the validity a <= floor(x * b / sB) fail) | stay rejected: NC145 |  | held (defence in depth) |
| DM-CP4 | KobCondPair | a bought-custody is KCC-20 only: defence in depth (a KRON T held as a custody is also refused by the custody's KCC-20 state equality, a KRON state starting with its owner push, not 0x08, and in a fill by mode == 0) | stay rejected: atrNM93, btrNM93 |  | held (defence in depth) |
| DM-CP5 | KobCondPair | mode 1 later fill: the bought-custody is the P2SH of the script its state is read from (a planted non-P2SH input of T's covenant id with the genuine custody redeem; on the public-mint and KaspaRocket programs the token program also refuses it) | atkNM08, btkNM08 | atkNM08(F) btkNM08(F) btkNM08 atkNM08 | CONFIRMED |
| DM-CP6 | KobCondPair | the bought-custody is known by its exact state (amount bought, plain owner = the order id, zero guard, the commitment) | atkNM01, btkNM01, atkNM02, btkNM02, atkNM03, btkNM03, atkNM04, btkNM04, atkNM07, btkNM07, atkNM05, btkNM05, atkNM06, btkNM06, atkNM19, btkNM19 | atkNM01 atkNM02 atkNM03 atkNM04 atkNM05(F) atkNM06(F) atkNM07 atkNM19 btkNM01 btkNM02 btkNM03 btkNM04 btkNM05(F) btkNM06(F) btkNM07 btkNM19 | CONFIRMED |
| DM-CP7 | KobCondPair | refund: the released bought-custody keeps its KAS | atkNM42, btkNM42 | atkNM42 btkNM42 | CONFIRMED |
| DM-CP8 | KobCondPair | mode >= 0 (hostile field: mode -1 would fill as mode 0) | atkNM51, btkNM51 | atkNM51 btkNM51 | CONFIRMED |
| DM-CP9 | KobCondPair | mode <= 2 (hostile field: mode 3 would fill as mode 0) | atkNM50, btkNM50 | atkNM50 btkNM50 | CONFIRMED |
| DM-CP10 | KobCondPair | a KRON T is delivered new: mode 0 only (NM90, mode 1, is not listed: its continuation keeps bought 0, which the continuation pin also refuses) | atrNM91, btrNM91, atrNM92, btrNM92 | atrNM91 atrNM92 btrNM91 btrNM92 | CONFIRMED |
| DM-CP11 | KobCondPair | mode 1: bought >= 0 (hostile field: bought -5 delivers a custody of n - 5, the matcher keeps 5) | atkNM67, btkNM67 | atkNM67 btkNM67 | CONFIRMED |
| DM-CP12 | KobCondPair | modes 0 and 2: bought == 0 (a mode-0 / mode-2 order with bought > 0 and its custody spent delivers n and leaves the custody's units to the matcher, NM52b / NM53b; without the custody, NM52 / NM53, the bought-custody check refuses it too) | atkNM52b, btkNM52b, atkNM53b, btkNM53b; stay rejected: atkNM52, btkNM52, atkNM53, btkNM53 | atkNM52b atkNM53b btkNM52b btkNM53b | CONFIRMED |
| DM-CP13 | KobCondPair | mode 2 forms 1 and 2: recvThreshold > 0 | atkNM62, btkNM62, atkNM77, btkNM77 | atkNM62 atkNM77 btkNM62 btkNM77 | CONFIRMED |
| DM-CP14 | KobCondPair | mode 2 forms 1 and 2: topupTip >= 0 (hostile field) | atkNM65, btkNM65 | atkNM65 btkNM65 | CONFIRMED |
| DM-CP15 | KobCondPair | mode 2: topupTip <= deliveryCarrier | atkNM64, btkNM64, atkNM86, btkNM86, atkNM87, btkNM87 | atkNM64 atkNM86 atkNM87 btkNM64 btkNM86 btkNM87 | CONFIRMED |
| DM-CP16 | KobCondPair | mode 2: form is 0, 1 or 2 (form 3 would fill as a top-up, NM63b; form 3 on a form-1 build, NM63, is also refused by the top-up's receive-state checks) | atkNM63b, btkNM63b; stay rejected: atkNM63, btkNM63 | atkNM63b btkNM63b | CONFIRMED |
| DM-CP17 | KobCondPair | mode 2 top-up: the receive UTXO is the P2SH of the script its state is read from | atkNM79, btkNM79 | atkNM79(F) btkNM79(F) btkNM79 atkNM79 | CONFIRMED |
| DM-CP18 | KobCondPair | top-up: the receive UTXO is the maker's | atkNM70, btkNM70 | atkNM70 btkNM70 | CONFIRMED |
| DM-CP19 | KobCondPair | top-up: the receive UTXO's threshold is at least recvThreshold | atkNM71, btkNM71 | atkNM71 btkNM71 | CONFIRMED |
| DM-CP20 | KobCondPair | top-up: the receive UTXO's guard is canonical (NM72) and it carries tExt: defence in depth for the commitment, since output i is pinned with tExt and the token program keeps the successor's commitment (NM73 held) | atkNM72, btkNM72; stay rejected: atkNM73, btkNM73 | atkNM72 btkNM72 | CONFIRMED |
| DM-CP21 | KobCondPair | a re-arming BID exit pays the maker exactly its S profit dOut at output i (the S twin of cond-pair-rpt CR1) | NCR1b | NCR1b NCR1c | CONFIRMED |
| DM-CP22 | KobCondPair | a re-arming BID exit's S profit is bound to S's covenant (an unbound copy of the profit script holds nothing; the profit goes to the matcher) | NCR1c | NCR1c | CONFIRMED |
| EVTPL | KobCondPair | evidence template hash and P2SH both removed: the wrong side (the order own counterparties) and the order / a token / a P2PK input are still refused (state read at the other template offsets fails the token / scale identity; the fill push refuses token / P2PK inputs) | stay rejected: NC26a, NC26b, NC54, NC55, NC56 | NC5Ha(F) NC5Sa(F) NC3Ha(F) NC3Sa(F) NC5Hb(F) NC5Sb(F) NC3Hb(F) NC3Sb(F) NC58 NC5P | held (defence in depth) |
| DM-CP1 | KobCondPair | a re-arming fill (the entry present) is mode 0 | yNM82 | yNM82(F) | CONFIRMED |
| DM-CP2 | KobCondPair | mode 2 top-up: output i is the receive UTXO's successor (DLV-07: a second order claims the receive UTXO another order tops up) | aNM20, bNM20 | aNM20 bNM20 | CONFIRMED |
| DM-CP3 | KobCondPair | mode 2 top-up: output i is the receive UTXO's successor (DLV-07, one order: the successor at a lower output, output i a copy of its state) | atkNM85, btkNM85 | atkNM85 btkNM85 | CONFIRMED |

Collateral flips (scenarios accepted besides the expected ones):

- G2: NC18t0 NC18t1 NC18t2 NC18t3 NC18t4 NC18t5 NC18t6 NC18t7 NC19t
- GS0: NC09a NC09b
- GS1: NC06a NC07a NC08a NC06b NC07b NC08b
- EVHASH: NC5Ha NC3Ha NC5Hb NC3Hb
- EVP2SH: NC5Sa NC3Sa NC5Sb NC3Sb
- DM-CP21: NCR1c
- EVTPL: NC5Ha NC5Sa NC3Ha NC3Sa NC5Hb NC5Sb NC3Hb NC3Sb NC58 NC5P

## ifd-pair

KobIfdPair pair order; test binary `kob_ifd_pair_tests`; contracts `contracts/orders`. 133/133 confirmed, 4/4 defence-in-depth held (expected NOT to flip).
Baseline (no mutation): ifd_fill_buy clean, ifd_fill_sell clean, ifd_update clean, ifd_custody clean, ifd_strays clean, ifd_encoding clean, ifd_exit_genesis clean, ifd_merge_rules clean, ifd_aliasing clean, ifd_evidence clean, audit_pair_sec_tests::aud_rb_ifd_pair_rearm_bound clean, ifd_merge clean, ifd_refund clean, ifd_outputs clean, ifd_hostile clean, ifd_fill_rules clean, ifd_refund_rules clean, ifd_ev_template clean, ifd_update_rules clean, ifd_stray_slots clean, ifd_custody_marks clean, ifd_ev_battery clean, ifd_ev_tk clean, audit_pair_sec_tests::aud_ext_ifd_pair_refuses_evidence_of_another_extension_commitment clean, ifd_merge_refund clean, ifd_pair_exit_delivery_modes clean, ifd_limits clean.

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
| IF-AUD-RB | KobIfdPair | while at least one minimum fill of re-arms is left, a fill takes at most rptAmount - 1 (it is booked): a larger fill would leave the remaining re-arms unused for good (buy-first and sell-first) | NIAUD-RB1, NIAUD-RB2 | NIAUD-RB1 NIAUD-RB2 | CONFIRMED |
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
| IF73x | KobIfdPair | heldGx KCC-20: the custody carries the extension commitment the entry names (escrow / prefund / A custody / merged exit; another commitment is another token; audit 2026-10-13: before, only kob_pair_custody_ext_tests asserted it) | NIH5k | NIH5k | CONFIRMED |
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
| IF120 | KobIfdPair | rd: a KCC-20 KAS-book bid of A names A's commitment (its extensionCommitment, read with its scale) | NAUD-EXT-I0b | NAUD-EXT-I0b | CONFIRMED |
| IF121 | KobIfdPair | rd: a KCC-20 custody of the ask / pair evidence is of its token's commitment (mode 0 the A ask; mode 1 the pair order's S) | NAUD-EXT-I0a, NAUD-EXT-I1a, NAUD-EXT-I1bB | NAUD-EXT-I1bB NAUD-EXT-I0a NAUD-EXT-I1a | CONFIRMED |
| IF122 | KobIfdPair | rd: the pair evidence's tExt is the commitment of its T (B of a pair ASK, A of a pair BID) | NAUD-EXT-I1aB, NAUD-EXT-I1b | NAUD-EXT-I1b NAUD-EXT-I1aB | CONFIRMED |
| IF123 | KobIfdPair | evLeg: a KCC-20 bid of B names B's commitment | NAUD-EXT-I0aB | NAUD-EXT-I0aB | CONFIRMED |
| IF124 | KobIfdPair | evLeg: the custody of a KCC-20 ask of B is of B's commitment | NAUD-EXT-I0bB | NAUD-EXT-I0bB | CONFIRMED |
| IF100 | KobIfdPair | stopTouch: evMode is 0 or 1 | NE21a1, NE21b1 | NE21b1 NE21a1 | CONFIRMED |
| IF101 | KobIfdPair | stopTouch: the B quote is positive (an ask of B at price 0 would arm any buy stop: ceil(stop * 0 / scale(B)) = 0) | NE22b0 | NE22b0 | CONFIRMED |
| IF102 | KobIfdPair | stopTouch: a sell stop arms only at a rate <= entryStop (one unit beyond refused; PE17 exactly at the stop accepted) | NE17a0, NE17a1 | NE17a0 NE17a1 | CONFIRMED |
| IF103 | KobIfdPair | stopTouch: a buy stop arms only at a rate >= entryStop | NE17b0, NE17b1 | NE17b0 NE17b1 | CONFIRMED |
| IF104 | KobIfdPair | merge: m > 0 (a merge never reads the exit refund / update push n = 0 as an amount). NIM9 (m = 0 next to a take-profit of n) stays rejected by the push check IF110 | NIM10; stay rejected: NIM9 | NIM10 | CONFIRMED |
| IF104c | KobIfdPair | NIM9 has the exit at input 1 (at input 0 a merge of 0 is the refund encoding 0). m > 0 AND the entry / exit push cross-checks removed: a merge of 0 next to a take-profit of n leaves the n bought A and back(n) to the matcher | NIM9 | NIX7 NIX7e NIM9 | CONFIRMED |
| IF105 | KobIfdPair | merge: the exit at k carries the committed exit state (outside the mutable payloads): a look-alike with another tpPrice refused | NIX1 | NIX1 | CONFIRMED |
| IF106 | KobIfdPair | merge: the exit is booked by this entry (push opcodes, parent, rptPrice = price, rptPre): another rptPrice refused | NIX2 | NIX2 | CONFIRMED |
| IF107 | KobIfdPair | merge: the committed exit is the entry's opposite side (KCC / KCC: a buy-first entry whose committed exit is a BID refused). On the mixed pairs NIX3m stays rejected by the exit's sExt check (B's commitment is not A's; IF107x / IF107c) | NIX3; stay rejected: NIX3m | NIX3 | CONFIRMED |
| IF107x | KobIfdPair | merge: the exit's sExt is the commitment of its custody token as fill writes it (aExt buy-first, bExt sell-first). DEFENCE IN DEPTH on NIX3m with the side check IF107; IF107c removes both. The check now also pins the delivery block [543..579), which the mutation keeps | stay rejected: NIX3m |  | held (defence in depth) |
| IF107c | KobIfdPair | the committed exit's side AND its sExt check removed: a buy-first entry merges a BID exit (mixed pairs: its sExt is B's commitment; the delivery block [543..579) stays pinned) | NIX3m | NIX3 NIX3m | CONFIRMED |
| IF108 | KobIfdPair | merge: the committed exit quotes at the entry's scale of A (an exit at scale 999 refused: the two would round the merge differently) | NIX4 | NIX4 | CONFIRMED |
| IF109 | KobIfdPair | merge: the exit's sigscript starts with an 8-byte push (a 9-byte push whose bytes [1..9) are m refused; the exit's own byte[8] argument refuses it too). NIX6 (a minimal push) stays rejected by the amount check | NIX5; stay rejected: NIX6 | NIX5(F) | CONFIRMED |
| IF110 | KobIfdPair | merge: the exit's pushed n is m. DEFENCE IN DEPTH with the exit's own check of the entry's push (KobCondPair, cond-pair-rpt CR7): an entry merging m != n stays rejected by the exit (NIX7, judged at the exit); IF110c removes both | stay rejected: NIX7 | NIX7e(F) | held (defence in depth) |
| IF110c | KobIfdPair | the entry's check of the exit's push AND the exit's check of the entry's push removed: an entry merging m = n - 1 leaves budget(1 whole A) and the exit's KAS to the matcher | NIX7, NIX7e | NIX7 NIX7e | CONFIRMED |
| IF111 | KobIfdPair | merge: the buy-first budget(m) is positive (a price-0 entry re-armed with no escrow growth refused) | NIM8 | NIM8 | CONFIRMED |
| IF112 | KobIfdPair | merge into an existing A custody: it keeps its carrier | NIM6 | NIM6 | CONFIRMED |
| IF113 | KobIfdPair | merge into an existing B custody: it keeps its carrier | NIM7 | NIM7 | CONFIRMED |
| IF114 | KobIfdPair | merge sell-out: the exit custody input xc is authenticated (heldGx): xc named at a P2PK input would shrink the entry's claim of the exit's KAS | NIX11 | NIX11 | CONFIRMED |
| IF115 | KobIfdPair | a booking needs n < 2^53 (the merge argument): a KCC-20 A booked for 2^53 refused (PIF12k books 2^53 - 1) | NIF12k | NIF12k | CONFIRMED |
| DM-IP1 | KobIfdPair | merge: the merged exit carries the entry delivery tail (exitMode, exitRecvThreshold, exitTopupTip, bought 0) | aNX11, aNX12, bNX11, bNX12, aNX13, aNX14, bNX13, bNX14 | bNX11 bNX12 bNX13(F) bNX14(F) aNX11 aNX12 aNX13(F) aNX14(F) | CONFIRMED |
| DM-IP2 | KobIfdPair | a repeating entry commits exitMode 0 | aNX08, aNX09, bNX08, bNX09 | bNX08 bNX09 aNX08 aNX09 | CONFIRMED |
| IF125 | KobIfdPair | rd: the A / pair evidence interval is not negative (an evidence one DAA old with interval -10^12 would arm the stop entry) | NE27a0, NE27b0, NE27a1, NE27b1 | NE27b0 NE27b1(F) NE27a0 NE27a1(F) | CONFIRMED |
| IF126 | KobIfdPair | evLeg: the B evidence interval is not negative (a B evidence one DAA old with interval -10^12 would arm the stop entry) | NE28a0, NE28b0 | NE28b0 NE28a0 | CONFIRMED |

Collateral flips (scenarios accepted besides the expected ones):

- IF9b: NI19
- IF8c: NIO9
- IF76: NIS9
- IF104c: NIX7 NIX7e
- IF107c: NIX3
- IF110: NIX7e

## cond-pair-rpt

KobCondPair repeat / merge (exit of KobIfdPair); test binary `kob_ifd_pair_tests`; contracts `contracts/orders`. 10/10 confirmed, 2/2 defence-in-depth held (expected NOT to flip).
Baseline (no mutation): cond_rpt_until clean, cond_rpt_profit clean, cond_rpt_rules clean, cond_rpt_merge_k clean, ifd_merge_rules clean.

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

## curve-ask

KCC-20 curve; test binary `kob_curve_ask_tests`; contracts `contracts/orders`. 44/44 confirmed.
Baseline (no mutation): curve_custody_negatives clean, curve_stray_guard_slots_8x8 clean, curve_created_in_the_token_genesis clean, curve_cancel_negatives clean, curve_negatives clean.

| id | contract | check ablated | expected | flipped | status |
|---|---|---|---|---|---|
| K1 | KobCurveAsk | tplRs: the custody input is the P2SH of the holder script it pushes (a planted UTXO) | NK2 | NK2 | CONFIRMED |
| K2 | KobCurveAsk | tplRs: the custody's template is the token's (a look-alike holder) | NK1 | NK1 | CONFIRMED |
| K3 | KobCurveAsk | noStrays bound: at most 8 token inputs (the 8 / 8 program refuses a 9th on its own) | NS8 | NS8(F) | CONFIRMED |
| K-S0 | KobCurveAsk | noStrays scan slot 0: a stray of the curve at token slot 0 rides to the taker | NS0 | NS0 | CONFIRMED |
| K-S1 | KobCurveAsk | noStrays scan slot 1: a stray of the curve at token slot 1 rides to the taker | NS1 | NS1 | CONFIRMED |
| K-S2 | KobCurveAsk | noStrays scan slot 2: a stray of the curve at token slot 2 rides to the taker | NS2 | NS2 | CONFIRMED |
| K-S3 | KobCurveAsk | noStrays scan slot 3: a stray of the curve at token slot 3 rides to the taker | NS3 | NS3 | CONFIRMED |
| K-S4 | KobCurveAsk | noStrays scan slot 4: a stray of the curve at token slot 4 rides to the taker | NS4 | NS4 | CONFIRMED |
| K-S5 | KobCurveAsk | noStrays scan slot 5: a stray of the curve at token slot 5 rides to the taker | NS5 | NS5 | CONFIRMED |
| K-S6 | KobCurveAsk | noStrays scan slot 6: a stray of the curve at token slot 6 rides to the taker | NS6 | NS6 | CONFIRMED |
| K-S7 | KobCurveAsk | noStrays scan slot 7: a stray of the curve at token slot 7 rides to the taker | NS7 | NS7 | CONFIRMED |
| K4 | KobCurveAsk | contSpk: the bytes rsLen reads are this input's redeem script (a wrong rsLen continues another script) | NC23 | NC23 | CONFIRMED |
| K5 | KobCurveAsk | the curve is the only input of its covenant id | NK3 | NK3 | CONFIRMED |
| K6 | KobCurveAsk | unbound: the custody comes from the order UTXO's own transaction | NB1 | NB1 | CONFIRMED |
| K7 | KobCurveAsk | only an unbound curve (tokenCovId zero) takes its token from the custody | NB3 | NB3 | CONFIRMED [positive scenarios rejected: PG3] |
| K8 | KobCurveAsk | the custody carries the curve's token (NB3, a bound curve's other-token custody, is also refused by the rest's covenant binding) | NK4 | NK4 | CONFIRMED |
| K9 | KobCurveAsk | the custody is owned by the curve's covenant id | NK5; stay rejected: NK6 | NK5 | CONFIRMED |
| K10 | KobCurveAsk | the custody has owner scheme 0x04 and borrowing disabled | NK7; stay rejected: NK6 | NK7 | CONFIRMED |
| K10x | KobCurveAsk | owner and scheme removed together: a key-owned custody | NK6 | NK5 NK6 NK7 | CONFIRMED |
| K11 | KobCurveAsk | the custody carries the curve's extension commitment | NK8 | NK8 | CONFIRMED |
| K12 | KobCurveAsk | the custody holds exactly amountLeft | NK9 | NK9 | CONFIRMED |
| K13 | KobCurveAsk | no fill before activeFrom | NC17 | NC17 | CONFIRMED |
| K14 | KobCurveAsk | n > 0 (a fill of 0 under minFill 0 re-spends the order) | NC19 | NC19 | CONFIRMED |
| K15 | KobCurveAsk | scale > 0 (hostile field: a negative scale sells for nothing) | NC20 | NC20 | CONFIRMED |
| K16 | KobCurveAsk | tip >= 0 (hostile field) | NC24 | NC24 | CONFIRMED |
| K17 | KobCurveAsk | amountLeft <= total (else the walk starts before level 0 at its price) | NC21 | NC21 | CONFIRMED |
| K18 | KobCurveAsk | minFill unless the fill takes all | NC18 | NC18 | CONFIRMED |
| K19 | KobCurveAsk | every level price covers the tip | NC22 | NC22 | CONFIRMED |
| K20 | KobCurveAsk | a fill ends within 8 levels (the 9th level free) | NC8 | NC8 NC8b | CONFIRMED |
| K21 | KobCurveAsk | the walk stops at every level boundary | NC5 | NC1b NC1c NC2 NC3 NC4 NC5 NC8b | CONFIRMED |
| K22 | KobCurveAsk | each level is rounded up for the maker | NC4 | NC1b NC4 | CONFIRMED |
| K23 | KobCurveAsk | positional payout: output i pays P2PK(maker) | NC16 | NC16 | CONFIRMED |
| K24 | KobCurveAsk | a partial fill: exactly one output of the curve's id | NC14 | NC14 | CONFIRMED |
| K25 | KobCurveAsk | the continuation is this script with amountLeft - n (and the token written) | NC6, NB2 | NC6 NC23 NB2 | CONFIRMED |
| K25b | KobCurveAsk | the first fill of an unbound curve writes its token into the continuation | NB2 | NB2 | CONFIRMED [positive scenarios rejected: PG2 PG4] |
| K26 | KobCurveAsk | the continuation keeps the order's carrier | NC13 | NC13 | CONFIRMED |
| K27 | KobCurveAsk | the rest custody keeps the custody's carrier | NC12 | NC12 | CONFIRMED |
| K28 | KobCurveAsk | a partial fill pays the maker the walk | NC1a, NC1b, NC1c, NC3, NC4, NC5, NC7 | NC1a NC1b NC1c NC3 NC4 NC5 NC7 | CONFIRMED |
| K29 | KobCurveAsk | the rest custody is pinned: the custody template with amountLeft - n owned by the curve | NC10 | NC10 | CONFIRMED |
| K30 | KobCurveAsk | the rest custody is bound to the token's covenant | NC11 | NC11(F) | CONFIRMED |
| K31 | KobCurveAsk | a sell-out leaves no output of the curve's id | NC15 | NC15 | CONFIRMED |
| K32 | KobCurveAsk | a sell-out pays the walk and both carriers | NC2 | NC2 | CONFIRMED |
| K33 | KobCurveAsk | cancel: SIGHASH_ALL only | NX2 | NX2 | CONFIRMED |
| K34 | KobCurveAsk | cancel: the maker's signature | NX1 | NX1 | CONFIRMED |

Collateral flips (scenarios accepted besides the expected ones):

- K10x: NK5 NK7
- K20: NC8b
- K21: NC1b NC1c NC2 NC3 NC4 NC8b
- K22: NC1b
- K25: NC23
