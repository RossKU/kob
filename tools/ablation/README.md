# Contract ablation (mutation) tooling

One runner, `ablate.mjs`, for the contract-source ablations of the harness suites: the KAS-quoted order kinds (KCC-20 and
KRON: sell side, buy side) and the pair orders (`KobPair`, `KobCondPair` with its repeat-IFD checks, `KobIfdPair`). Node 22, no dependencies, no `cargo` unless you do not pass `--exe-dir`. Works on Windows, Linux and macOS (the
repo root is derived from the script location, `tools/ablation/../..`).

Latest results: [RESULTS.md](RESULTS.md). Pair phase (2026-10-06): `pair` 82/82 confirmed + 2 held, `cond-pair` 112/112 + 10 held; `ifd-pair` (120 entries) and `cond-pair-rpt` (11) are partly run (67 confirmed / held in earlier runs, 87 entries pending in their current form, see RESULTS.md). Protocol v3 KAS kinds (2026-10-05/06): 438 mutations, **424/424 confirmed, 14/14 held** (kcc20-sell 92/92 + 3, kcc20-buy 90/90 + 4, kron-sell 139/139 + 3, kron-buy 103/103 + 4).

## What ablation proves

The harness suites in `crates/kob-tests/tests/` push transactions through the real script engine. Every attack scenario
(`run_bad`) must be REJECTED. A passing suite does not say which check did the rejecting; it could be another check, a
malformed transaction, or a bug in the scenario. Ablation answers that: remove exactly one check from a copy of the
contract source, rerun the suite, and see which attacks are now ACCEPTED.

A mutation is **confirmed** when

1. the unmutated contracts reject the attack (the baseline, see `--baseline`), and
2. with the check removed, every named attack scenario is accepted by every input (`all_inputs_ok=true`), in every
   template section of the run (the KRON suites loop over both pinned KRON templates; a mutation must flip on both).

A few checks are defence in depth (two independent checks bind the same thing). A mutation may list scenarios under
`hold`: removing this check alone must NOT flip them (they must be reached and still rejected). A mutation with only
`hold` entries has the status `HELD`; a mutation with both `expect` and `hold` is `CONFIRMED` when both hold. Example:
the exit script equality and the exit genesis id of the KRON sell-first entry both bind the booked exit; removing either
alone leaves the booked-exit attacks rejected, removing both flips them (`IA24a`, `IA24b`, `IA24`).

## The two hooks (crates/kob-tests)

| Env var | Effect |
|---|---|
| `KOB_ABLATION=1` (any value) | `run_bad` prints `ABLATION-PASS <scenario name> all_inputs_ok=<bool>` and returns, instead of failing, when the attacked input is accepted (every suite). `run_ok` of every suite (KCC-20 sell / buy, KRON sell / buy, the pair suites) prints `ABLATION-POS-FAIL <name> (...)` and returns when a positive scenario is rejected by a weakened contract, instead of panicking. Without `KOB_ABLATION` a rejected positive scenario still panics. Rejected attacks print `NEGATIVE <name> [REJECTED ...]` as always. |
| `KOB_ABLATION_SRC=<dir>` | `common::contract_source(name)` reads `<dir>/<name>.sil` if that file exists, else the committed `contracts/**/<name>.sil`. The committed contracts are never touched. |

The runner sets both for every run and uses the repo root as working directory. The executables find the repo through
the compile-time `CARGO_MANIFEST_DIR`, so build them from the same tree you run the tool in. To keep the runs
independent of anything else, the runner copies every `.sil` of the suite's contract directory into the scratch
directory (line endings normalised to LF) and mutates only the copies.

## Suites

| Suite | Family | Test binary | Contracts (`srcDir`) | Contracts mutated | Mutations |
|---|---|---|---|---|---|
| `kcc20-sell` | KCC-20, sell side: repeat IFD lifecycle, touch trigger, v3 checks (minFill, rounding, tip, merge push) | `kob_v2_tests` | `contracts/v2` | KobAsk, KobBid, KobCondAsk, KobIfdBid | 95 |
| `kcc20-buy` | KCC-20, buy side: repeat IFD lifecycle, touch trigger, v3 checks | `kob_v2_buy_tests` | `contracts/v2` | KobCondBid, KobIfdAsk | 94 |
| `kron-sell` | KRON adapter, sell side (v3) | `kob_kron_v2_tests` | `contracts/adapters/kron/v2` | KobAskKron, KobBidKron, KobCondAskKron, KobIfdBidKron | 142 |
| `kron-buy` | KRON adapter, buy side (v3) | `kob_kron_v2_buy_tests` | `contracts/adapters/kron/v2` | KobCondBidKron, KobIfdAskKron | 107 |
| `pair` | pair orders, plain (`KobPair`, both sides, both families of A and B) | `kob_pair_tests` | `contracts/v2` | KobPair | 84 |
| `cond-pair` | pair conditionals (`KobCondPair`): legs, trigger evidence in both modes, trailing, updates | `kob_cond_pair_tests` | `contracts/v2` | KobCondPair | 122 |
| `cond-pair-rpt` | the repeat-IFD checks of `KobCondPair` (TP fills with / without the entry, re-arm profit, budget, prefund) | `kob_ifd_pair_tests` | `contracts/v2` | KobCondPair | 11 |
| `ifd-pair` | pair if-done entries (`KobIfdPair`, buy-first and sell-first, merges, stop entries) | `kob_ifd_pair_tests` | `contracts/v2` | KobIfdPair | 120 |

The pair suites run every scenario on token pairs of both families (`common/pair_harness.rs`: the three pair templates are
compiled from source in dependency order, KobCondPair against the compiled KobPair and KobIfdPair against both, so a
mutation of KobPair is also seen where a resting KobPair is the trigger evidence of a conditional, and a mutation of
KobCondPair where an entry creates its exit). The cross limit catalogs (`kcc20-cross`, `kron-cross`) are gone with
`KobCross` (retired in the pair phase).

Each suite is a catalog in `catalogs/<suite>.mjs` exporting `suite` (`name`, `family`, `testBin`, `srcDir`,
`templateMarker` = regex for the `=== KRON template <file>` marker or `null`, `expectedTemplates` = number of template
sections one run must show: 2 for KRON, 1 for KCC-20) and `mutations`. A test reference in a mutation (`test`, the keys of
`run` and `hold`) is a test fn of the suite's binary, or `<bin>::<fn>` for a test fn of another kob-tests binary (a
KobPair check whose attack is a conditional armed by a fake pair quote, in `kob_cond_pair_tests`).

Mutation ids are unique per suite only (`A1` exists in `kcc20-sell` and `kron-sell`); `--only` accepts `id` or
`suite:id`.

### Touch trigger mutations (protocol v2.6)

A stop leg, a trailing ratchet and a stop entry arm only in a transaction that fills a plain resting KobAsk / KobBid of the
same token (the evidence), read by `touchAsk(ev, tk)`, `touchBid(ev)` or `touch(ev, tk)` (update, either side). The checks
are the same in every contract of both families, so `lib/touch.mjs` generates one mutation per check, scoped to the function
by its header: `<prefix>-push08` (the evidence sigscript starts with the 8-byte push of n), `-npos` (n > 0), `-tpl` (template
hash only; the P2SH check kept), `-p2sh` (P2SH only; the template hash kept), `-token`, `-scale` (the evidence quotes per the
same scale), `-mintouch` (n >= minTouch base units), `-slope`, `-cltv` (tx.daa >= exposed + minRestDaa), the three exposure terms `-interval`, `-active`,
`-tokdaa`, and for ask evidence `-tkcov` / `-owner` (the custody input carries the token and is owned by the evidence ask).
Hand-written entries cover the side / price rules in settle and update (`-rule`), the trailing step (`TT-k` / `TW-k`: k = 0
would drain keeperTip, k < 0 would move the stop back), the auction that opens at the trigger (`TI-auc` / `TK-auc`) and
update's own-token guard (`-own`, or the re-pointed `I14` / `CB3` / `IA3` entries in the KRON catalogs).

Prefixes (sell / buy catalogs): `TA` KobCondAsk stop leg armed in settle, `TU` KobCondAsk update (`TU-*` flips both the
arm scenarios `NTU*` and the trail scenarios `NTT*`), `TI` KobIfdBid buy-stop entry (`NTI*` fill, `NTJ*` update); `TB`
KobCondBid buy-stop leg armed in settle, `TV` KobCondBid update (`NTV*` arm, `NTW*` trail), `TK` KobIfdAsk sell-stop entry
(`NTK*` settle, `NTL*` update). The scenario numbers are the same in every battery (`crates/kob-tests`, `touch_battery`):
01..04 exposure (UTXO, custody, activeFrom, interval; exactly R accepted, R - 1 refused), 02b a fresh custody hidden behind an
old token UTXO of another token, 05 slope, 06 side, 07 / 07b token, 08 another scale, 09 minTouch (one base unit below refused), 10 a cancel whose signature is
ground to read as n > 0, 11 / 11b refund / n = 0, 12..14 ev at the order, a token input, a P2PK input, 15 / 16 tk at a token
input the evidence ask does not own / another ask's custody (fresh real custody), 17 look-alike template, 18 non-P2SH redeem,
19 / 19b the price edge, 20 one fill arming two orders, 21 the order's own counterparty, 22 conditional / if-done fills, 23
update's own-token guard. The receipt mutations of the KRON catalogs are gone (`I2`..`I4`, `IA8b`); `I1`, `I7`, `I14`,
`CB3`, `IA3`, `IA8`, `IA8c` now remove the touch call, the price rule or the own-token guard.

## Usage

```
node tools/ablation/ablate.mjs [--suite kcc20-sell|kcc20-buy|kron-sell|kron-buy|pair|cond-pair|cond-pair-rpt|ifd-pair|all] [--only ID,ID]
     [--jobs N] [--exe-dir DIR] [--out DIR] [--dry-run] [--baseline] [--no-baseline] [--list] [--keep]
     [--release] [--timeout SEC]
```

| Option | Meaning |
|---|---|
| `--suite` | one suite, a comma list, or `all` (default) |
| `--only` | run only these mutations (`A1,C7` or `kron-sell:A1`) |
| `--jobs N` | parallel test processes, default 4 |
| `--exe-dir DIR` | use the newest `<bin>-<hash>[.exe]` in DIR (e.g. `target/debug/deps`). Without it the runner calls `cargo test -p kob-tests --test <bin> --no-run --message-format=json` (once per binary; add `--release` for release executables) and takes the reported executable. |
| `--out DIR` | scratch and result directory, default `<repo>/target/ablation` |
| `--dry-run` | apply and validate all edits, run nothing; the mutated sources are left in `--out` for inspection |
| `--baseline` | run each distinct (suite, test fn) once without mutation and require zero `ABLATION-PASS`, zero `ABLATION-POS-FAIL`, a clean exit, the full number of template sections, and a `NEGATIVE` line for every scenario id the catalog expects. Runs nothing else. |
| `--no-baseline` | skip the baseline that a normal run does first. A mutation whose test fn has a dirty baseline is reported `BASELINE-DIRTY` and not run. |
| `--list` | print ids, contract, test fn, expected scenarios and notes |
| `--keep` | keep the scratch directories of confirmed mutations (failed ones are always kept: mutated `.sil`, `edits.txt`, one `<testFn>.log` per run) |
| `--timeout SEC` | kill a test run after SEC seconds, default 1800 |

Examples:

```
node tools/ablation/ablate.mjs --list --suite kron-buy
node tools/ablation/ablate.mjs --dry-run --suite all --out /tmp/abl
node tools/ablation/ablate.mjs --suite kron-sell --only A1,C17 --jobs 2 --exe-dir target/debug/deps
node tools/ablation/ablate.mjs --baseline --suite all --exe-dir target/debug/deps
node tools/ablation/ablate.mjs --suite all --jobs 6            # full run, cargo builds the test executables
```

Exit codes: `0` everything as expected; `1` at least one mutation not confirmed, an edit error, a dirty baseline; `2`
usage error or executables not found.

## How a run works

1. Load the catalogs, apply `--suite` / `--only`.
2. For every mutation: copy the suite's `.sil` files to `<out>/<suite>/<id>/`, apply the edits to the mutated contract
   (and the `also` contracts). Every edit must match exactly once and must change the text, or the mutation is an
   `EDIT-ERROR` (never a silent no-op).
3. Locate the executables (`--exe-dir` or cargo).
4. Baseline runs (unless `--no-baseline`).
5. For every (mutation, test fn): run `<exe> --exact <test fn> --nocapture --test-threads=1` with `KOB_ABLATION=1
   KOB_ABLATION_SRC=<scratch>`, cwd = repo root, at most `--jobs` at a time.
6. Parse stdout per template section, decide the verdict, print one line per mutation, write `results.md` and
   `results.json` into `--out`, print `N/M confirmed`.

Scenario ids are matched by the first whitespace-delimited token of the scenario name (`NRP10 booked exit ...` is
`NRP10`), so a trailing space in a catalog id does not matter.

## Reading the results

Console line: `<STATUS> <suite>:<id> [<contract>] <expected ids> :: <flipped per template> (<seconds>)`.

| Status | Meaning |
|---|---|
| `CONFIRMED` | every expected scenario was accepted by all inputs in every template section |
| `HELD` | defence in depth: none of the listed scenarios flipped and all were reached (`NEGATIVE` line present) |
| `NOT-CONFIRMED` | reason follows: `still rejected` (another check also rejects it, or the edit removed the wrong thing), `not reached` (the test aborted first: panic, compile error of the mutated contract, wrong scenario id), `partial` (accepted, but not by all inputs, shown as `ID(F)`), or a wrong number of template sections. Defence-in-depth entries fail with `flipped ..., expected to stay rejected` |
| `EDIT-ERROR` | an anchor or pattern no longer matches the committed source exactly once, or the edit is a no-op: fix the catalog entry |
| `BASELINE-DIRTY` | the unmutated contracts already accept an attack (or the run is broken); the mutation was not run |

`results.md` has one table per suite: id, contract, check ablated, expected, flipped scenarios per template, status.
"Flipped" lists every scenario accepted in that run; scenarios beyond the expected ones are collateral (checks that
guard the same attack twice, or a check shared by several attacks) and are listed under the table. `(F)` marks a flip
that not all inputs accepted. Panics after the expected flips (for example a positive scenario that the weakened contract
now rejects) are noted in the status. In every suite these show up as `positive scenarios rejected: ...`.

## Adding a mutation

Append an object to the `mutations` array of the suite's catalog:

```js
{
  id: 'C7',                              // unique within the suite, no whitespace
  file: 'KobCondAskKron',                // contract mutated (name of the .sil without extension)
  test: T_STOP,                          // test fn ...
  expect: ['NK1'],                       // ... and the attack scenario ids that must flip
  // or several tests: run: { [T_A]: ['NA1'], [T_B]: ['NB1', 'NB2'] },
  note: 'keeper takes at most keeperTip',
  edits: [ del('        require(tx.outputs[selfOut].value >= tx.inputs[self].value - keeperTip);\n') ],
  // also: [{ file: 'KobIfdBidKron', edits: [ ... ] }],   // extra edits in other contracts (double removals)
  // hold: { [T_X]: ['NX9'] },                            // scenarios that must stay rejected (defence in depth)
  // inputOnly: ['NX3'],                                  // accepted by the attacked input, other inputs reject anyway
}
```

Edit ops (`lib/edits.mjs`; helpers `del`, `rep`, `rx`, `rxDel`, `fn` build the object forms):

| Op | Object form | Semantics |
|---|---|---|
| `del(s)` | `{ del: s }` | remove `s`; must occur exactly once |
| `rep(from, to)` | `{ replace: [from, to] }` | replace `from`; must occur exactly once |
| `rx(anchor, re, to)` | `{ regex: { anchor, pattern, to } }` | `anchor` must occur exactly once; the first match of `re` at or after it, before the next `entry` / `function` declaration, is replaced by `to` (string or `m => string`) |
| `rxDel(anchor, re)` | `{ regex: { anchor, pattern, to: '' } }` | same, deleting the match and its line break |
| `fn(label, f)` | `{ fn: f, label }` | callback `text => text`, for anything else; still checked for no-op |

Guidance: prefer the smallest edit that removes one check; write the anchor so that it is unique (`--dry-run` tells you);
pick `expect` ids from the scenario names in the test (`run_bad(... "NK1 ..." ...)`), list only the scenario that the
check is meant to catch (other flips are reported as collateral); when two independent checks bind the same thing, make
one entry per check listing the scenarios under `hold`, and one entry removing both (`also`, or two ops) with the flips
expected. `inputOnly` lists scenario ids for which `all_inputs_ok=false` is acceptable: the attack transaction is
rejected on another input by a different check (`KB-P3b` / `KI-P3b`: 5 token inputs, rejected by the token program on
input 1 whatever the order does), so the proof is that the order's own input (0) flips. Such flips show as `ID(F)`.
A mutation of a contract that a test function does not exercise cannot flip, and shows as `not reached` or
`still rejected`.

## Runtime

Debug-build executables, one machine: an earlier full run (before the cross-limit suites were added; 16 baseline runs plus 155 mutations in 165 test runs) took 22 minutes with `--jobs 6`; the six-suite run of 2026-09-30 (247 mutations) took 2,575 s with `--jobs 2`; the run in `RESULTS.md` (440 mutations, v2.6) took 1,847 s + 1,295 s + 2,985 s in three invocations with `--jobs 3`; the protocol v3 runs (438 mutations, one suite per invocation, `--jobs 2`) took 736 s (kcc20-sell), 1,165 s (kcc20-buy), 1,430 s (kron-sell) and 2,693 s (kron-buy). One test run takes 5 to 120 s (the KRON tests run both
templates); the `kron-buy` negative, lifecycle, touch and repeat-IFD suites are the slowest. Baseline of
everything: about 2.5 minutes with 6 jobs. The runs are CPU-bound and independent, so `--jobs` near the physical core
count is the right setting.

## Relation to the other ablation work

* **Lifecycle review of protocol v2.3** (the project's own pre-release review, "36 of 36 ablations confirmed"): the `kcc20-sell` and
  `kcc20-buy` catalogs are those 36 ablations (formerly an in-place perl edit of `contracts/v2` with one `cargo test`
  per ablation; that script is not in this repo), now run against scratch copies. Scenario ids follow the current test names (`NRP*` / `NRPB*`; the old
  script said `NR*` / `NRB*`). Two mutation controls of that review are included: `M2` (auction
  `require(t >= newArmed)` in KobCondAsk, witness `NA3`; and in KobCondBid, witness `NB29`, in `kcc20-buy`) and `M4`
  (`require(keeperTip >= 0)` in KobCondAsk, witness `NK2`). The harness patching the earlier control script did is unnecessary since
  `KOB_ABLATION` exists. That script removed the `t >= newArmed` check of both contracts in one go; here it is one entry
  per contract because the two live in different test binaries.
* **KRON adapter ablations**: `kron-sell` and `kron-buy` unify two earlier ad-hoc scripts (not in this repo: one with exact-once string
  edits, one with anchor + regex edits, parallel workers, `special: 'pin'` which is now an `rxDel` on the
  `validateOutputStateWithInputTemplate(...)` call after the `Pin the only token output` comment). Entries `IA24a` and
  `IA24b` are the defence-in-depth pair (`hold`); the old scripts guessed their scenario lists, the catalog now holds the
  measured ones.
* **Argent router** (`crates/kob-tests/tests/argent_router_tests.rs`, `router_ablation_*`): ablates the generated router
  source inside the test itself. Each ablation edits one entry and asserts that every needle occurs exactly once, so it
  cannot silently be a no-op. That in-test variant is kept as is; this tool covers only the contract-source ablations of
  the order suites.

Checks without a witness attack (for example `keeperTip >= 0` in the other three conditional/IFD contracts) are not in
the catalogs; a mutation needs a scenario that covers it.
