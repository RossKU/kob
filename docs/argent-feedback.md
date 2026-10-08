# Notes for the Argent team

KOB uses Argent in two ways (see [argent.md](argent.md)): its order contracts are hand-written
SilverScript published as the Argent app `KOBOrders`, and its composition layer (the swap-and-pay
router) is written in Argent and imports `KOBOrders` and `KOBToken`. Doing that we met things that
may be gaps in Argent, gaps in our understanding, or both. Each item gives where it shows up and,
where we have one, a reproduction. **We may well be wrong on any of them, and there may be a better
way; corrections are very welcome.**

Everything below is against the pinned upstream commit `b312deda6fe10f6493c8d49eb748e3f61860458a`
(master of 2026-09-30, after the module loading rework #62; SilverScript v1.0.0) plus our two
patches (`argent/UPSTREAM.md`); every compiler-dependent result was measured on it. To rebuild that
`argentc`:

```
argent/build-argentc.sh          # clone, pin, patch, build; prints the path of argentc
scripts/build-argent.sh --check  # builds contracts/argent with it and checks the committed output
```

The code is pre-audit and Argent is pre-1.0; nothing here is meant as criticism of either.

## Items

Status tags: **as designed** (confirmed by the Argent team), **planned upstream**, **patched**
(fixed for KOB by a patch in `argent/patches/`), **open**, **closed**. Where an item carries an
"Answer", it is the Argent team's reply of 2026-10-01.

| Item | Topic | Status |
|---|---|---|
| 1 | `co_spent()` is presence-only | as designed |
| 2 | observed token outputs without a `become` block | as designed |
| 3 | no generated protection against output aliasing | as designed (app level) |
| 4 | importers of an artifact do not verify the imported handles | patched (0002) |
| 5 | `observes` fixes the exact input and output count | planned upstream |
| 6 | the 244 live stack binding limit | open (measured) |
| 7 | comparing a stored `actor_type` with an imported template | planned upstream |
| 8 | a KCC-20 token actor inside a multi-actor app | closed (separate app kept) |
| 9 | all entries of an actor go into one redeem script | not planned for the first release |
| 10 | size of a 1:1 port of hand-written orders | measured |
| 11 | building against rusty-kaspa v2.1.0 | patched (0001), upstream open |
| 12 | closed ICC import of an artifact without source | patched (0002), PR prepared |
| 13 | `argent-runtime` in the browser | closed |
| 14 | an artifact does not prove which compiler built it | closed |
| A1-A4 | smaller notes | open |

### 1. `co_spent()` is presence-only (status: as designed)

An entry whose only visible effect looks harmless (we had a `top_up`) can still let a transaction
builder redirect tokens, because `co_spent()` only states that a covenant input is present, and
`argentc` gave no hint.

* Answer: intended; the author's responsibility. We account for it when writing entries.

### 2. An entry that observes token outputs but has no `become` block (status: as designed)

Such an entry compiles, and the observed token outputs end up unconstrained by the entry itself.

* Answer: intended. We account for it when writing entries.

### 3. No generated protection against output aliasing (status: as designed, app level)

An observed group pins the shape of a group, but which input "owns" which output position is left
to the author. Without a rule, one output can satisfy two intents' checks at once (a self-trading
payer, two intents sharing one delivery). In our tests a naive two-intent transaction let a builder
keep about 10 KAS. We wrote the rule by hand: the payout for input `i` sits at output `i`; in the
router an intent owns outputs `j` and `j + 1` and must be the last input.

* Where: `tools/router-gen/router_head.ag` ("POSITIONAL ANTI-ALIASING RULE"),
  `docs/argent.md` ("Positional anti-aliasing rule").
* Repro: `crates/kob-tests/tests/argent_router_tests.rs`, the ablation cases `A-K7`, `A-K8`
  (with the rule removed the attack transactions validate; on the shipped router they reject).
* Answer: app-level business logic, no compiler help expected. The rule stays hand-written and
  reviewed. It may be worth a line in Argent's docs as a known pattern.

### 4. Importers of an artifact do not verify the imported actor handles (status: patched, 0002)

Needed only for artifact-backed imports (Argent's own follow-up "Support artifact-backed app
dependencies"). With a source import argentc compiles the exporter and derives the handle itself,
so there is nothing to verify. With an artifact import the handle comes from the artifact: argentc
does not check a linked handle against the exporter's template receipt, the interface fingerprint
excludes the handle, and `check_consistency()` only shows that the id is the content hash. Whoever
supplies the artifact then chooses which template the importer accepts. Patch 0002 (item 12) adds
the verification we would expect: the import names the artifact id it reviewed
(`import "./x/artifact.json" id "<id>";`), and any other artifact is refused. Receipt checks
("Verify linked actor receipts") would be a second layer, not a replacement: a forged artifact can
have consistent receipts.

* Answer (to the question "where exactly would you expect a verification that does not currently
  exist?"): the above.

* Where: `docs/argent.md` ("Trust"), `crates/kob-tests/tests/argent_import_pin_tests.rs`.
* Repro: `argent_import_pin_tests.rs` builds `contracts/argent/examples/closed_icc_gate.ag` against
  the genuine artifact (links its handle), a forged self-consistent artifact with the same app name
  and ABI (refused under the pin), without a pin (refused), and against the interface SOURCE
  `KOBOrders.ag` (compiles, and links the handle argentc derived from that source, which is not the
  published one).

### 5. `observes` fixes the exact input and output count (status: planned upstream)

An observed group lists its inputs and outputs by name, so a sweep over one to three orders needs
one entry per shape, and because of item 9 one actor per shape. KOB's router has 18 actors for
three payment intents; `tools/router-gen` writes them from one pattern.

* Where: `contracts/argent/kob_router.ag` (`actor KasToToken_buy2`, `_buy3`, `TokenSwap_swap2`,
  ...), generated by `tools/router-gen/gen-router.sh`; the reasoning is in
  `tools/router-gen/router_head.ag` ("FILL SHAPES") and `docs/argent.md` ("Fill shapes").
* Repro: an entry observing `escrows: KOBToken::KCC20[1..=3]` in a token group fails with "entry
  `A::go` declares range `escrows`, but range code generation is not implemented yet".
* Answer: will be added before the first release; the router generator is then unnecessary.

### 6. The 244 live stack binding limit (status: open, measured)

The order states have 19 and 21 fields. Copying a state into a local costs one live binding per
field, against the 244 live bindings allowed per entry, so the router reads fields by projection.
In the 2 + 2 swaps the continuation pins do not fit.

* Repro: generate the router with the continuation pins (`PIN=1 tools/router-gen/gen-router.sh FILE`
  adds `require book.outputs become { next <- ... }` for every resting order; `PIN=ask` only for
  the resting ask, `PIN=bid` only for the resting KCC-20 bid) and compile it (SilverScript v1.0.0,
  `3ed97333`, the release upstream pins since #60). argentc stops at the first failing actor:
  `generated Silverscript for actor TokenSwap_swap2 failed to compile: variable '...' requires 245
  live stack bindings, exceeding the consensus limit of 244`. That message names the first binding
  over the limit, not the peak.
* Exact peaks (2026-10-03, router after the rewrite for every token program; a build of the same
  compiler that records the peak instead of stopping, its shipped scripts byte-identical to the
  release build), live bindings / combined stack items, limit 244 / 244:

  | actor | shipped | + ask pin | + bid pin | + both |
  |---|---|---|---|---|
  | `TokenSwap_swap2` | 227 / 234 | 248 / 259 | 250 / 263 | 271 / 282 |
  | `TokenSwapKron_swap2` (KRON bids are not pinned) | 217 / 224 | 238 / **249** | 217 / 224 | 238 / **249** |

  Every other shape fits with both pins (next largest: `TokenSwap_swap2_out` 216 / 223, it has no
  resting order). The ask pin costs 21 bindings and 25 stack items in the script, the bid pin 23 and
  29: the 19 or 21 fields of the continuation state plus the template prefix / suffix of the
  `become` check. The shipped router therefore still leaves the continuation pin out of every shape:
  the orders enforce their continuation themselves (`docs/argent.md`, "Fill shapes").
* Protocol v3 (2026-10-05, amounts in base units, no lot arithmetic in the router): with the same compiler the
  ask pin alone now compiles in every shape (`PIN=ask`); the bid pin still stops `TokenSwap_swap2` at 245 combined
  stack items and both pins at 245 live bindings. The shipped router still pins neither.
* Most of these bindings are state fields nothing reads: `readInputStateWithTemplate` decodes every
  field of every observed state. With a SilverScript change that drops unused field reads (prepared
  as an upstream proposal; not used by KOB), `TokenSwap_swap2` peaks at 164 / 171 shipped, 199 / 210
  with the ask pin, 202 / 215 with the bid pin and 237 / 248 with both (both pins together are still 4
  combined stack items over), `TokenSwapKron_swap2` at 190 / 201 with the ask pin, and the whole
  router shrinks from 177,207 to 113,223 bytes (`swap2` 9,999 to 6,266).
* Question: is there a recommended idiom (or a compiler improvement) for large observed states?

### 7. Comparing a stored `actor_type` with an imported template (status: planned upstream)

Argent issue #61, raised earlier by another user: a stored `actor_type` cannot be compared against
an imported template, so one order cannot hold handles for two token families. Our cross-token
order needs exactly that.

* Where: `KobCross` in `contracts/argent/KOBOrders.ag` (state `KobCrossState`) and
  `contracts/v2/KobCross.sil`: token A and token B are stored as plain template hash plus prefix
  and suffix lengths (`tokenTplHash`, `tplPrefixLen`, `tplSuffixLen`, `bTplHash`, `bPrefixLen`,
  `bSuffixLen`) rather than as `actor_type` handles, which is what we would use if the comparison
  were possible.
* Answer: work in progress. We will switch to the Argent form when it lands.

### 8. A KCC-20 token actor inside a multi-actor app (status: closed, separate app kept)

An actor in a multi-actor app gets compiler-owned template context in its state, so its program
differs from the single-actor build.

* Answer: the template can change with the surrounding app, which is why the artifact exposes
  `actor_type_handle`, a stable template view over the user-level state (`KCC20State`) that
  importers use. Measured on `b312ded`, it does not cover our case:

* The KCC-20 actor of `contracts/argent/kcc20_8x8.ag` built together with a second actor in one app:
  the state span grows from 1 + 112 to 1 + 145 bytes (the context field `gen__kcc20_template`), the
  `transfer` entry takes two more hidden witnesses (it reads its delegates with the template), and
  the program grows from 6,820 B to 9,864 B. Its `actor_type_handle` is a correct view of THAT
  program (state `KCC20State`, context field `gen__kcc20_template`, prefix 34 B, suffix 9,718 B,
  template `3d08058a...`), which is not the token KOB issues.
* KOB's token program is fixed outside Argent: it is `KCC20Ref_8x8` (template `40fef59a...`), which
  the token UTXOs on chain carry and which every KOB order pins as `tokenTplHash`. A handle can only
  describe the program its app compiled; it cannot make a multi-actor build produce, or stand for,
  the deployed program. The single-actor app `KOBToken` compiles to exactly that program, so its
  handle is the deployed template. `KOBToken` therefore stays its own app; this costs nothing,
  the router imports it like any other app.
* Repro: append a second actor and an `app` listing both to `kcc20_8x8.ag`, build, and compare
  `sil/KCC20.sil` and the artifact's `template_plan.templates[KCC20].actor_type_handle` with
  `contracts/argent/KOBToken/`.

### 9. All entries of an actor go into one redeem script (status: not planned for the first release)

The redeem script of a covenant UTXO carries every entry of its actor, so an actor with many
entries makes every spend reveal all of them. We split the router to one actor per shape
(2.5-9.4 kB of redeem script per shape; a cancel reveals only its own actor: 3.0 kB and 0.006 KAS
for `KasToToken`, 3.8 kB and 0.008 KAS for `TokenToKas`, 6.2 kB and 0.012 KAS for a single-order
swap).

* Answer: per-entry script splitting is a future vector, not for the first release.

### 10. Size of a 1:1 port of hand-written orders (status: measured)

Question from the Argent team: what exactly is duplicated by a port?
`contracts/argent/port/kob_ask_port.ag` is `KobAsk` (`contracts/v2/KobAsk.sil`) with the body
verbatim, plus what Argent requires (`emits next: KobAsk[0..=1]` and a bulk `become` of the
continuation state, `amountLeft - n`, when the ask rests). One forced edit: `self` is reserved (the
local is renamed). The hand-written order uses `if` statements, not `a ? b : c` (which argentc's
lexer rejects), so nothing else is rewritten. `crates/kob-tests/tests/argent_port_tests.rs`
compiles both with the same constructor arguments and removes each check on its own to measure it
(`cargo test -p kob-tests --test argent_port_tests -- --nocapture`):

| | Bytes |
|---|---|
| hand-written `KobAsk` (protocol v3, with the custody's `extensionCommitment`) | 1,684 |
| 1:1 port | 2,807 (+1,123) |
| generated: `become` (`cont.length == count`, `validateOutputState` of the continuation) | 514 |
| port: the continuation state literal `become` needs (20 fields, `amountLeft - n`) | 593 |
| generated: output count bounds of `settle` (`0 <= count <= 1`) | 8 |
| generated: `OpAuthOutputCount == 0` in `cancel` | 5 |
| port without all four | 1,687 (hand-written + 3) |
| body checks the generated ones repeat: `OpCovOutputCount(selfId) == 1` / `== 0` (x2) | 21 |
| body checks the generated ones repeat: continuation SPK `== contSpk(amountLeft - n)` | 81 |
| port without the repeated body checks (an idiomatic port) | 2,705 (+1,021) |

So the duplication is small: **102 B**, the order's own output-count checks (21 B, duplicated by the
generated count bound and `cont.length == count`) and its continuation SPK check (81 B, duplicated
by the generated `validateOutputState`). An idiomatic port drops them and is still +1,021 B. That
difference is not duplication but the mechanism: `become` rebuilds the whole 20-field state
(593 B) and re-encodes and compares it (514 B), where the hand-written order splices one 8-byte
field into its own script (`contSpk`: the redeem script with bytes [236..244) replaced, 81 B in the
port, 79 B in the hand-written order). The cost of `become` grows with the number of state fields.
The 3 B of "port without all four" are the generated local `gen__next_output_count`.
Opcode-level listings of the hand-written order and the idiomatic port, section by section, and the
splice prototype: [argent-port-compare.md](argent-port-compare.md).

* Question: could a `become` whose state differs from `self` only in some fields compile
  to a splice of those fields (or offer an opt-in "continuation = self with field := value")? For
  KOB's order shapes that is the whole ~1 kB. If this is not of interest we withdraw the point.

### 11. Building against rusty-kaspa v2.1.0 (status: patched, 0001; upstream open, #64)

Upstream at `b312ded` still builds against rusty-kaspa `a41a333b`; the Toccata mainnet node is
v2.1.0. Patch 0001 (Argent issue #64) moves the dependencies to `tag = "v2.1.0"`, points
`silverscript-*` at our patched vendored copy and adapts two lines of `crates/argent-runtime`
(`MAINNET_PARAMS.block_mass_limits`, covenants always enabled). It applies to `b312ded` unchanged.

* Where: `argent/patches/0001-argent-rusty-kaspa-v2.1.0.patch`, `vendor/silverscript/UPSTREAM.md`.
* Answer: to be decided (#64).

### 12. Closed ICC import of an artifact without source (status: patched, 0002; PR prepared)

Artifact-backed imports are still open upstream at `b312ded` ("Support artifact-backed app
dependencies"). Patch 0002, rebased onto the module loading rework (#62), adds an optional pin to
a module import: `import "./KOBOrders/artifact.json" id "<artifact id>";` loads a published
artifact (consistency-checked, its id must be the pinned one), turns its states and exported
actors into a declaration-only module (`KOBOrders::KobAsk` and `KobAskState` resolve as for a
source import) and links it without compiling it. Upstream-style tests in the patch show that the
importer's artifact is identical to the one built against the source import, and that a forged or
unpinned artifact, a pin on a source import, a tampered artifact and an artifact as the root are
refused.

* Where: `argent/patches/0002-argent-artifact-backed-app-imports.patch` (exactly the PR diff),
  `contracts/argent/examples/closed_icc_gate.ag`, `tools/router-gen/router_head.ag`.
* Answer: a PR is welcome. It will be opened from a fork; the draft text explains the pin (item 4)
  and the follow-ups it leaves open (project configuration, receipt checks, artifacts with their
  own dependencies).

### 13. `argent-runtime` in the browser (status: closed)

`argent-runtime` has no wasm support, so KOB's browser flows use their own Rust library
(`kob-protocol` through `kob-wasm`). An earlier claim of ours that the runtime needs a "~5 MB
wasm" in the browser came from our own experimental wasm32 compile of it (`build()` runs the full
script engine), not from anything Argent ships, and is withdrawn.

### 14. An artifact does not prove which compiler built it (status: closed)

Answer: artifacts are not self-verifying by design; a trusted provider or a full compiler
validation is required. KOB does the latter: `argent/build-argentc.sh` builds the pinned `argentc`,
`scripts/build-argent.sh --check` rebuilds the committed artifacts and fails on any difference, and
`sil2argent verify <artifact.json> --id <artifact id> ...` re-derives what a published artifact
claims without `argentc` (`tools/sil2argent`). An informational compiler version field would be
nice to have but is not needed.

## Additional notes

Smaller points; the same caveat applies, we may have missed a supported way to do each.

### A1. Publishing hand-written SilverScript as an Argent app (status: open)

KOB's order contracts are hand-written, and we want other Argent apps to observe them by template
hash. We declare an interface app with placeholder bodies, compile it with `argentc` to a skeleton
artifact, and let `tools/sil2argent` replace each generated contract by the `silverc` artifact of
the hand-written source, recompute the template receipts, handles, interface fingerprints and the
artifact id, and refuse the result if state layouts or entry signatures differ.

* Where: `contracts/argent/KOBOrders.ag`, `tools/sil2argent/src/lib.rs`, `scripts/build-argent.sh`.
* Repro: `scripts/build-argent.sh --check`, or `cargo test --locked -p sil2argent`.
* Concern: this depends on artifact internals (`vendor/argent-artifact`), so it may break when
  they change. We are unsure that this is a pattern Argent wants to support.
* Question: is there, or will there be, a supported way to import a hand-written SilverScript
  contract into an Argent app?

### A2. Hidden witnesses of imported handles (status: open)

An entry that observes an imported actor takes hidden witnesses (the handle's prefix and suffix
lengths). A keeper has to know them to build the sigscript.

* Where: `docs/argent.md` ("Importing KOBOrders", step 3; router table). The harness builds the
  sigscripts by hand: `crates/kob-tests/tests/common/mod.rs` (`encode_entry_sig_script`).
* `argent-runtime`'s `TxBuilder` has not been validated on the v2 artifacts, so this may already
  be solved there.
* Question: which is the intended way for a non-Argent keeper to learn the witness layout from the
  artifact?

### A3. Absolute paths in artifacts (status: open)

`argentc` records canonical absolute source paths in its output (`root`, `modules`, and each
contract's `source_path`). They are informational and excluded from the artifact id, but the
committed bytes differ by checkout location and operating system. We stage the build inside the
repository (a path without symlinks or 8.3 short names) and strip the checkout directory
afterwards (`sil2argent relativize`).

* Where: `scripts/build-argent.sh` ("Stage inside the repo ..."), `tools/sil2argent/src/main.rs`.
* Repro: build the same app from two directories and diff the two `artifact.json` files.
* Question: could paths be recorded relative to the source root?

### A4. Open ICC and state names (status: open, unverified now)

For an open handle (`actor_type<KobAskState>`) we redeclare the state field for field.
In an earlier test `argent-runtime` matched an open handle to an actor by the state name, so the
published name had to be used, although on chain the check is structural.

* Where: `contracts/argent/examples/open_icc_gate.ag` (header), `docs/argent.md` ("Open ICC").
* Not re-checked on the current pin; please treat it as a question, not a bug.

## What we can share

Patches, the harness (`crates/kob-tests/tests/argent_router_tests.rs`: positive transactions,
negative cases and ablation pairs, run in rusty-kaspa v2.1.0's script engine) and the ablation
results (`tools/ablation/RESULTS.md`), if any of it is useful to Argent's own tests.
