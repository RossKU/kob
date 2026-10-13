# Token programs: profiles, onboarding, review

Status: 2026-10-13. Normative for this repository. KOB pairs with every KCC-20 launchpad (the KRON launchpad, Kasseal,
KaspaCom, KForge, future ones), and each brings a token program or a build of a shared one. Supporting a new program must be a
small, well-defined, reviewable step. This document defines the **token-program profile** (everything KOB needs to know about
one program, declared in one place), the onboarding path (`kob registry add-program`), the generic genesis rule for launchpad
genesis shapes, and the checklist a reviewer works through before a program's tokens can be listed.

## 1. The profile

Every token program KOB embeds has one entry in [`crates/kob-protocol/data/programs.json`](../../crates/kob-protocol/data/programs.json)
([`kob_protocol::profile::ProgramProfile`](../../crates/kob-protocol/src/profile.rs)). Nothing about a program is declared
anywhere else in code: `build.rs` generates its `TemplateId` variant and embeds its bytes from the entry, and every consumer
reads the entry.

| Field | Meaning | Consumed by |
|---|---|---|
| `name` | Program name (`[A-Za-z0-9_]+`): the `TemplateId` name and serde spelling, the key of the budget and tip tables, the kob-wasm and web program name | everything |
| `variant` | Rust `TemplateId` variant (generated) | Rust code and tests |
| `registry_id` | Template id in `registry/tokens.json` (`templates[].id`) | registry, web |
| `record_code` | One-byte program code of the executor's record log. Persisted: never reused or renumbered | indexer (`indexer/record.rs`) |
| `summary` | One line | docs, `kob registry programs` |
| `source` | `kind` (`silverc-artifact` or `raw-redeem`), repo-relative `path`, optional `context_fields` (compiler-owned state fields the KCC-1 actor-type handle moves into the prefix) | `artifacts.rs` (embedding, cut) |
| `template_hash` | blake3 `template_hash(prefix, suffix)` of the cut, lowercase hex: the pin | `artifacts.rs`, order states, registry |
| `layout` | `prefix_len`, `state_len`, `suffix_len` of the cut | every redeem script KOB derives or recognises |
| `codec` | State codec family, `kcc20` (112-byte state) or `kron` (46-byte state) | state codec, the order templates of that family |
| `owners` | `custody` (owner value of order custody, a covenant id) and `holder` (owner value of wallet holdings and maker deliveries) | custody equal to the family's order templates, holder one a build of them delivers to (checked); the holder selects the build (KRON: 3, or 0 for the `*KronD0` builds) |
| `slots` | `inputs` / `outputs`: token inputs and outputs one transaction may carry (leader included) | builders, planner (`matcher/family.rs`), keepers, router |
| `carrier_floor` | Smallest KAS value (sompi) of a token output the program accepts (measured) | builders, order-term checks (`check_new_order`) |
| `witness` | `scheme` (`kcc20-transfer` or `kron-columns`) and, for `kron-columns`, `trailing_pushes` (constant pushes after the witness column, e.g. an entry selector); `kcc1-tagged` (with `entries`) only in a derived, non-tradable profile (onboarding, step 1) | `tx.rs` signature scripts, the indexer's next-state parser |
| `router_intents` | Whether router intents may name the program | `router::intent_program` |
| `genesis` | `minters` (`genesis-only` or `unknown`) and `companions` (section 3) | `registry::verify_genesis`, `genesis_evidence` (condition C2) |
| `review` | `status` (`reviewed`, `pending-review`, `test-only`) and its `basis` | equal to the shipped registry's `review_status` (checked) |
| `networks` | The networks the program's tokens exist on (empty: any). A program not on mainnet is not on the shipped (mainnet) registry and no mainnet build trades it; a KRON program with holder 0 must name its networks, without mainnet | registry tests, `kob place` / web open markets |

The compute budgets and keeper tips are measured, not declared: `data/compute_budgets.json`, `data/keeper_tips.json` and
`data/pair_keeper_tips.json` are generated over every profile (`KOB_REGEN=1`, the test list `common::PROGRAMS` is
`TemplateId::TOKEN_PROGRAMS`) and keyed by `name`. A program without budgets cannot be built for (`MissingBudget`).

The web app reads the same table through `web/src/kob/programs.generated.ts` (`npm run gen:programs`; a test fails when it is
stale). `kob registry programs [--json]` prints the embedded table.

### Checks

* **Shape** (`ProgramProfile::check_shape`, on load and in `add-program`): the codec's state length; owners the family's
  order templates serve (KCC-20: custody 4, holder 0; KRON: custody 2, holder 3, or holder 0 with the testnet-only `*KronD0`
  builds, so a holder-0 KRON program lists its networks without mainnet); a witness scheme the family's builders
  implement; trailing pushes only for `kron-columns`; companions either a known decoder of the right role or an opaque
  program (a minter needs a decoder); a non-empty review basis.
* **Table**: unique names, variants, registry ids, record codes and template hashes.
* **Bytes** (`ProgramProfile::check_bytes`, on every load, `artifacts::self_check`): the cut of the source has the declared
  layout and template hash, its state span is a canonical state of the codec, and a `kcc20-transfer` artifact has the draft
  `transfer` / `transfer_delegator` entries with their dispatch tags (`79c71c23`, `fd3ef14a`).
* **Registry**: a strict-list entry whose template hash is an embedded program must state the profile's family, layout, slots
  and custody owner (`profile::check_registry_template`, run by `Registry::validate`); the shipped registry's review status of
  every program equals the profile's, and every non-`test-only` program on mainnet is on it, no other (`profile` tests).

## 2. What is data and what is code

A program of an existing codec family is **data plus review**: its bytes under `contracts/`, its profile, its registry entry,
regenerated budget and tip tables, and the review below. No Rust, TypeScript or contract source changes.

Code is needed only for:

* a new **codec family**: a new state layout, or owner values the existing order templates do not use. One
  [`CodecFamily`](../../crates/kob-protocol/src/profile.rs) implementation (state length, custody and holder owners, witness
  schemes, output amount bound, state recognition), its order templates (a KOB order kind pins the token program by template
  hash, prefix and suffix length as state fields, but the owner values and the state layout are compiled in) and their review
  and ablation;
* a new **witness scheme** (a new way to authorise a token input): a builder in `tx.rs` / `script.rs` and the indexer parser;
* a new **companion decoder** (a genesis actor whose state bounds the supply, like the public-mint app's `PublicMint`):
  `profile::match_companion` and the decoder.

## 3. Launchpad genesis shapes

A KIP-20 covenant id commits to the outputs of its genesis group, and a token program reads balances and authority from any
input carrying its covenant id, so a hidden non-template output in the group is genesis contamination
([`registry/README.md`](../../registry/README.md), "Genesis check"). Launchpads put more than token outputs in their launch
transactions. Two cases:

* **Companions bound to another covenant id** need no rule. They are not in the token's group: KIP-20 binds an output to the
  covenant id it names, and `verify_genesis` recomputes the token's id over the outputs bound to it. Kasseal's launch carries
  the curve's genesis (its own covenant id) and the creator's change beside the two token outputs; KForge's curve ("rung") is
  its own covenant. The token outputs are then typically owned by the curve's covenant id (KCC-20 owner scheme 4, KRON
  id_type 2): `crates/kob-tests/tests/kcc20_public_mint_genesis_tests.rs`
  `a_kasseal_launch_genesis_verifies_as_a_fixed_supply_public_mint_token`.
* **Companions bound to the token's own covenant id** are accepted only when the program's profile declares them
  (`genesis.companions`), by role: `minter`, `seed`, `curve`, `pool`, `vault`. A companion is either *decoded* (`decoder`:
  `kcc20-public-mint/PublicMint` and `kcc20-public-mint/TokenSeed`, the issuance actors of the published public-mint app; a
  minter's `remaining` is summed into `mint_allowance`) or *opaque* (`template_hash` plus `layout`: recognised by the template
  hash of its cut, reported in `GenesisReport::companion_outputs`, carrying no amount and no mint authority). Anything else in
  the group is refused (`NotTemplate`). `registry::verify_genesis_with` checks a group under a candidate profile, so a
  launchpad's genesis shape can be tried before its profile is committed (`registry` test
  `a_genesis_group_holds_the_companions_its_profile_declares_and_nothing_else`).

Condition C2 (no live mint authority) follows from the genesis alone only for a program whose profile says
`minters: genesis-only` (no transaction creates a minter without spending one); `unknown` leaves `live_minters` to the traced
evidence.

## 4. Onboarding a token program

```
kob registry add-program <ARTIFACT.json | REDEEM.bin | REDEEM.hex | HEX> --name NAME [--registry-id ID] [--slots I/O]
                         [--source-path PATH] [--upstream TEXT] [--write]
```

1. **Derive.** `add-program` reads a `silverc` artifact (cut from its state span, leading `gen__*` context fields moved into
   the prefix) or a raw redeem script observed on chain (the state span detected at offsets 0..=8 against the known codec
   families; an `OP_TOALTSTACK` prefix with an `== 0` dispatch after the state is recognised as an entry selector and becomes
   a trailing `OP_0` push). A program with KCC-1 entry tags (`silverc` v1: the `OP_TOALTSTACK` prefix and, after the state,
   `OP_FROMALTSTACK (OP_DUP <4-byte tag> OP_EQUAL OP_IF OP_DROP <entry> OP_ELSE)... OP_RETURN OP_ENDIF...`, walked opcode by
   opcode; from an artifact, its ABI's entries) gets witness `kcc1-tagged` with its entries in dispatch order (tag and, when
   a known signature recomputes to the tag, the KCC-1 `FunctionSignature`, i.e. its argument layout before the tag). No
   builder implements `kcc1-tagged`: such a profile describes the bytes only, gets no registry entry, and `--write` refuses
   it (the table refuses it too), so the program is never tradable. It prints the profile, the registry strict-list entry
   (`pending-review`) and the review tasks the bytes cannot settle. What the bytes do not decide gets the safe default: `pending-review`, `router_intents: false`,
   `minters: unknown`, `carrier_floor: 1` (to be measured), slots from `--slots` (to be measured).
2. **Write.** `--write` adds the bytes, appends the profile to `programs.json` (existing entries untouched) and writes the
   conformance stub `crates/kob-tests/tests/program_<name>.rs`: one live test (the embedded bytes match the profile) and one
   ignored test per review task. The program is then embedded and pinned but not tradable: it has no budgets, and it is not on
   the strict list.
3. **Registry.** Add the printed entry to `registry/tokens.json` `templates` as `pending-review` (with provenance under
   `source`, licence and `UPSTREAM.md` next to the bytes), declare its `capabilities` and `risks`.
4. **Measure.** Replace the ignored stub tests with engine tests: owners, slots, carrier floor (KForge below is a
   worked example), then `KOB_REGEN=1 cargo test -p kob-protocol --test budget_table --test keeper_tips` and the budget,
   tip and cost tests.
5. **Review** (section 5). Only then `review.status: reviewed` in the profile and `review_status: reviewed` in the registry,
   and `scripts/build-deploy.sh mainnet` (the deployment record pins the registry).
6. Run the full suite: `cargo test --workspace`, `cd web && npm run gen:programs && npx vitest run`.

## 5. Reviewer checklist

A reviewer signs off a program only when every item has a test or a written answer in the program's `UPSTREAM.md` / review
notes:

1. **Provenance**: the bytes are the program launchpad tokens actually use (a chain UTXO's redeem script, or an artifact
   that reproduces one); licence recorded; the template hash recomputed from the bytes.
2. **Layout**: the profile's cut is the program's state span (a real token UTXO's P2SH is the P2SH of `prefix ‖ state ‖
   suffix`); every context field is constant per program.
3. **Witness**: a transfer built by KOB's builder for the profile's scheme reproduces a real transfer (or is accepted by the
   engine), and every tampered variant (missing trailing push, wrong witness index, wrong next-state column) is rejected.
4. **Owners**: custody (covenant id) is spendable only by a transaction holding an input of that covenant; the holder owner
   the family's order templates deliver to is spendable by its owner and only by it. A program that refuses either cannot use
   the existing order templates (deliveries would be stuck): stop, this is a codec variant (section 2).
5. **Conservation and authority**: a non-minter transfer conserves the amount and creates no minter; every entry other than
   the transfer is listed with what it lets someone do (registry `capabilities`: mint authority, public mint, burn, freeze,
   seize, blacklist) and the declared `risks`.
6. **Limits**: slots and carrier floor measured in the engine and equal to the profile; the family's output amount bound
   (KRON: 1e9) accepted.
7. **Strays**: the program never lets a co-input of the same covenant id be spent without running its checks, and a UTXO
   owned by the token's own covenant id is accounted for (KOB never creates one).
8. **Genesis**: `minters` justified; every companion a launch puts in the token's group declared with its role (decoded or
   opaque) and shown to carry no balance the program would count, or a decoder written; a real launch genesis of the
   launchpad verified with `kob registry verify-genesis`.
9. **Order suites and ablation**: the KAS-book and pair order suites run with the program, and the ablation suites
   (`tools/ablation`) still confirm every mutation with tokens of this program.
10. **Budgets and tips** regenerated and the cost tests green; `router_intents` decided.

## 6. Worked examples

| Launchpad | Program | What onboarding needed |
|---|---|---|
| Kasseal | `kcc20-ref-public-mint` (no new program) | Nothing new: its launches use the reference's published public-mint build. Curve and pool are their own covenants, outside the token's genesis group; the SEAL launch genesis verifies as a fixed supply |
| KaspaCom 0.2.5 | `kcc20-kaspacom-0-2-5` | Data: profile with `carrier_floor` 0.5 KAS (measured), `router_intents: false`, `minters: unknown` (minter lanes behind the extension commitment). Stays `pending-review` (finding K-1) |
| KRON launchpad | `kron-2433`, `kron-2732` | The KRON codec family (orders and builders, reviewed B2); both programs are data on it |
| KForge (testnet-10) | `KForgeToken1785` | Code: the delivery id_type 0 builds of the KRON order kinds and id_type 0 holder spends (a token-level signature in the signature column). Data: profile with holder 0, slots 1 / 3, `networks: [testnet-10]`, `pending-review`; see below |

### KForge

`crates/kob-tests/tests/program_kforgetoken1785.rs`, fixtures in `contracts/third-party/kforge/` (`program.bin`, a redeem script
observed on testnet-10; `observed-transfer.json`, a real transfer).

* Derived from the bytes alone (`kob registry add-program ... --slots 1/3 --holder 0 --network testnet-10`): KRON codec, a one-byte
  prefix (`OP_TOALTSTACK`: an entry selector) before the 46-byte state, template hash
  `5b136633944a2371a1d0f6a94fdc0d329327ffc700fdec097fc59e76e1d33ae1`, witness `kron-columns` with one trailing `OP_0`. KOB's
  builder with that profile reproduces the signature script of the observed transfer byte for byte, and its token outputs are the
  P2SH of the derived cut.
* Measured in the engine: custody (id_type 2) released by the order covenant's input is accepted, and refused without the
  selector push, without the covenant's input, or when it mints; 1 token input and 3 token outputs per transaction; a carrier
  floor of 1 sompi; amounts up to 1e9 (and, unlike the KRON programs, a zero-amount output, which KOB never builds); holders are id_type 0, authorised by the owner's SIGHASH_ALL signature of the token input in
  the signature column (the witness byte is not read for them; no signature, another key's or another input's signature is
  refused); **address presence (id_type 3) is refused**. No minting, no burning, no minter output on the transfer branch, and no
  other selector value moves a holder's tokens without its signature, releases a custody without its covenant or mints.
* Because KOB's KRON order templates deliver to makers with id_type 3 (`TYPE_DELIVERY`), a KForge maker could never spend such a
  delivery: the KRON kinds have a second build with `TYPE_DELIVERY = 0x00` (`KobAskKronD0` .. `KobIfdAskKronD0`, a build parameter
  of the same sources, `contracts/adapters/kron/README.md`), and the profile's `owners.holder` selects it
  (`TemplateId::for_program`; an order's build follows from the token program its state pins, an instance of the other build is
  not recognised). The builders spend id_type 0 holdings with the owner's signature (`SigPlan::KronToken.signer`, a sign request
  for the token input with its redeem script, as for a KCC-20 holder input: KasWare's `signPskt` and Kaspire's `signPskt` with a
  `scripts` entry sign it, the app assembles the script). An id_type 0 input is its token's only input in a transaction (the
  program's slots are 1 / 3 anyway). The pair templates deliver KRON tokens with id_type 3: they refuse the program.
* Still open before `reviewed`: what the other entries (selector branches) let the launchpad do (`minters: unknown`, capabilities);
  a real KForge launch genesis verified with `kob registry verify-genesis` (the curve "rung" is its own covenant); the
  hand-written KRON order suites and their ablation run with the KForge program and the `*KronD0` builds (the builder scenarios
  over every program already run on it in the engine, with the shapes that fit 1 / 3 slots); a testnet-10 registry entry
  (`templates`, `delivery_id_type: 0`) for an operator who lists its tokens.

## 7. Invariants

* Template hashes of the programs are their profiles' `template_hash`; the order kinds stay pinned in `artifacts::PINNED`. A
  profile change that changes a hash is a protocol change.
* `record_code` values are persisted by every indexer: never reuse or renumber one (1..=9 are taken).
* The `TemplateId` variant order is the order kinds of the KCC-20 family, the KCC-20-codec programs, the KRON order kinds,
  the KRON-codec programs, each in table order: append new programs at the end of their codec group.
