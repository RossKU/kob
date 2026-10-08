# KCC base-spec conformance (KCC-1, KCC-2, KCC-20)

Status: checked on 2026-10-02 against the Last Call texts. KOB handles any token through adapters, but its own KCC-20
programs, its issuance and every encoding it labels KCC must conform to the KCC base specifications. This file records
what was checked, against which upstream bytes, what did not conform and what changed, and what still depends on
upstream.

## 1. Pins

| Spec | Upstream (kaspanet/kccs) | Status there | Vectors in this repo |
|---|---|---|---|
| KCC-1 Covenant Concepts, Byte Layouts, and ABI | `main` `411b41b` (compliance rewrite #27, Last Call #32) | Last Call | `crates/kob-tests/vectors/kcc1/conformance.json` |
| KCC-2 Authority Schemes | `main` `411b41b` (#30: unkeyed P2PKH hash, scheme ranges; Last Call #33) | Last Call | `crates/kob-tests/vectors/kcc2/authority-schemes.json` |
| KCC-20 Fungible Token | PR #31 head `cfb74cf` (Draft on `main`) | Last Call on the PR | `crates/kob-tests/vectors/kcc20/conformance.json` |
| KCC-20 reference program | argent-lang/kcc20-reference PR #1 head `600646873e` (unchanged on 2026-10-02) | unmerged | `contracts/kcc20/KCC20Ref.sil` (verbatim) |

Every vector file is vendored unmodified (CC0) with a `PROVENANCE.md` and pinned by sha256 in the test that runs it.

## 2. Results

`cargo test -p kob-tests --test kcc1_conformance_tests --test kcc2_conformance_tests --test kcc20_conformance_tests`

### KCC-1

| Section | Vectors | KOB code checked | Result |
|---|---|---|---|
| 3.2.1 Hash function | 3 | `kcc1::hash`, `kcc1::hash_keyed` (`Key32`), the engine's `OP_BLAKE3` / `OP_BLAKE3_WITH_KEY` | pass |
| 3.4.1 Canonical type names | 11 | `kcc1::type_name` / `parse_type_name` (SilverScript `bytes` = `byte[]`) | pass |
| 3.4.2 Data pushes | 13 | `script::push_data` (KOB's argument and redeem push), `kcc1::push_minimal` / `push_explicit`, silverscript-abi argument and state encoders, `kcc1::decode_arguments` | pass, 0 to 65,536-byte payloads |
| 3.4.3-3.4.4 Integers, scalars | 18 | silverscript-abi argument and state encoders, `kcc1` int payloads, decoders | pass |
| 3.4.5-3.4.6 Arrays, records | 9 | silverscript-abi encoders (record lowering, record-array grouping), decoders | pass |
| 3.5.1 Dispatch tags | 8 | `kcc1::function_signature` / `dispatch_tag`, and the SilverScript compiler (each vector compiled as a real entrypoint) | pass |
| 3.6 P2SH envelope | 1 | `script::p2sh_spk`, `kcc1::split_invocation`, `kcc2::p2sh_authority` | pass |
| 3.7.1 State encoding | 1 | silverscript-abi state encoder, `kcc1::decode_state` | pass |
| 3.7.3 Template hashes | 4 | `silverscript_abi::template_hash` and an independent `Hash(LE64 ‖ prefix ‖ LE64 ‖ suffix)` | pass |
| 3.7.4-3.7.5 Views, continuation | 3 + 1 | view cuts, view hashes, `EncodeState`, `R_next`, P2SH | pass |
| 3.9.1 Virtual element | 1 | `encode_struct_payload` (Packed), `kcc1::hash` | pass |
| Rejections | 30 | `kcc1::decode_arguments`, `kcc1::decode_state`, `kcc1::validate_argument_type`, `kcc1::check_entrypoints`, the encoders, the compiler | all rejected |
| Every KOB program | 55 programs, 159 entrypoints (`contracts/artifacts`, the router) | dispatch tags recomputed from the ABI types, every type a KCC-1 type, template hashes recomputed, state spans canonical | pass (one recorded exception, 3c) |

### KCC-2

| Section | Vectors | KOB code checked | Result |
|---|---|---|---|
| 2.1 Registry | 10 (+ all 256 bytes) | `kcc2::classify`, issuance owner-scheme set, x402 `owner_proof`, and the programs (a successor `owner_scheme` of each byte) | pass: `0x00`-`0x04` accepted, `0x05`-`0x7f` and `0x80`-`0xff` rejected |
| 4.1 Constructions | 5 | `kcc2::p2pkh_authority` (unkeyed `Hash(pubkey)`), `p2sh_authority`, `p2sh_envelope`, push forms, the KCC-20 state `owner` / `owner_scheme` bytes | pass |
| 4.2 Approval checks | 20 | `kcc2::{signature_approval, p2sh_approval, covenant_approval}`; the same cases executed in `KCC20Ref` (3/3) and `KCC20Ref_8x8` (the issued program) with real signatures (the vector keys are 1G, 2G, -G) | pass; 19 executed per program, `covenant-output-only` refused by construction |

### KCC-20

Unchanged: PR #31 is still at `cfb74cf`, the vendored vectors pass as before (state, dispatch, standard transfer, owner
witness for schemes `0x00`-`0x04`, borrow witness, hash chain, borrowed receive).

## 3. Mismatches found

| # | Where | What | Impact | Fix |
|---|---|---|---|---|
| a | `kob-x402` `token::leader_next_states` | Decoded the `State[]` amounts as two's complement (KCC-1 `int` is 8-byte signed magnitude) and gave up when a one-element field group was pushed as `OP_1`..`OP_16` (the KCC-1 `PushMinimal` form of a single successor with `owner_scheme` `0x01`-`0x04` or `borrow_scheme` `0x01`-`0x03`) | Diagnostics only (acceptance is by recomputation): a less precise rejection reason | Decodes with the strict KCC-1 decoder (`kob_protocol::kcc20::transfer_next_states`) |
| b | `kob-executor` indexer `kcc20_leader_next_states` | Same two's-complement amount decoding | None in practice (accepted transactions only, the programs require amounts >= 0) | Same shared decoder |
| c | Router (`crates/kob-protocol/data/router_sil_abi.json`) | The Argent router's intent states declare `deadline` as SilverScript `temporal`, which is not a KCC-1 type name | State bytes are identical to KCC-1 `int`; it is a state field only (no entrypoint, so no dispatch tag) | Recorded and allowlisted in the sweep; the type comes from SilverScript/Argent upstream |
| d | Router `IntentState::decode` | Used silverscript-abi's state decoder alone, which accepts non-canonical bytes (negative zero, an over-long push prefix); KCC-1 3.7.1 requires the consumed bytes to equal the canonical encoding | A malformed intent state could decode | Re-encodes and compares (as the order and token state codecs already did) |
| e | Issuance (`issue.rs`) | Owner schemes checked as `<= 0x04`; correct, but not tied to the registry | None | Constants are the `kcc2` bytes; the refusal names the KCC-2 range (reserved or custom) |

No KOB-owned program, template hash, registry entry, Argent handle, budget, tip, vector or deploy file changes:
everything KOB encodes or executes already conformed.

Upstream observations (not KOB code): the vendored SilverScript compiler rejects the three KCC-1 invalid argument types
and the truncated dispatch-tag collision; silverscript-abi's state decoder alone accepts negative zero and over-long
push prefixes, so every KOB caller re-encodes to check (`kcc1::decode_state`, `StateCodec`, the router).

## 4. The token programs

KCC-2 #30 replaced the keyed `Hash(pubkey, "PublicKeyHash")` with the unkeyed `Hash(pubkey)`. The upstream reference
program (argent-lang/kcc20-reference PR #1, `600646873e`) computes `p2pkh_hash` as `blake3(public_key)`, which compiles
to `OP_BLAKE3`, the unkeyed hash, and KOB's copies are byte-for-byte that program (`KCC20Ref.sil`; the slot variants,
`KCC20P2`, `KOBToken` and the wallet-gate copy derive from it). So there is no program change, no new template hash and
no template change; TN10 soak tokens are unaffected.
`crates/kob-tests/tests/kcc2_conformance_tests.rs` (`kcc20_programs_use_the_unkeyed_p2pkh_hash`) tracks this: it fails
if a keyed form returns or the provenance commit moves.

## 5. Pending on upstream

| Item | Current upstream | Expected change | Effect on KOB |
|---|---|---|---|
| KCC-20 PR #31 not rebased onto KCC-1 #27 / KCC-2 #30 | Cites KCC-1 "Section 9.1" (now 3.8.1, leader and delegator roles) and "Section 10" (now 3.9, virtual elements); writes `P2PKHHash(pubkey)` where KCC-2 now says `Hash(pubkey)` | Section references and notation | None on bytes: its vector values for `P2PKHHash` are already the unkeyed BLAKE3 (`7caa514a...` = `Hash(22^32)`), which the KCC-20 test asserts |
| KCC-20 record name | Default configuration names the record `KCC20State`; KOB's artifacts name it `State` | Possibly a name alignment | None: dispatch tags omit record names (`transfer` `79c71c23`, `transfer_delegator` `fd3ef14a`) |
| KCC-20 merge / vector re-issue | PR #31 at `cfb74cf`, KCC-20 Draft on `main` | Merge, possibly new vector bytes | Re-vendor `vectors/kcc20/` (the sha256 pin fails on any change) and rerun |
| Reference program | kcc20-reference PR #1 at `600646873e` | A new commit | Rerun section 4; if bytes change, regenerate the pinned artifacts (orders under the old template become unsupported: [template-retirement.md](template-retirement.md)) |

Observation for upstream: the `covenant-id/v1` minimum check also passes when `owner` is the token's own covenant id
(`covenant-self` is approved, and KOB's programs approve it as the vector requires), so such a state can be moved by any
transaction that spends it. KCC-2 section 6 warns that participation alone is not approval; KCC-20 PR #31 does not
forbid this owner. KOB issuance cannot create it (the covenant id is derived from the genesis outputs).

## 6. Optional batch leader (proposal for upstream, not issued)

The default configuration compiles `max_token_inputs` / `max_token_outputs` = 3 into the holder program, and every token
input pushes the whole program, so larger limits make every transfer dearer (1 -> 1 payment: 3/3 0.00702 KAS, 8/8
0.01448, 16/16 0.02642). `contracts/kcc20/p2/KCC20Opt.sil` (generated from the reference by
`kob_protocol::kcc20::kcc20_opt_holder_source`) keeps holders on the 3/3 program and adds an optional batch-leader
commitment: constructor constants `B_TPL` / `B_PRE` / `B_SUF` (template hash and prefix / suffix lengths). The
delegator path, instead of the reference's unused read of the leader state, accepts as leader its own template (P2SH
checked) or, when the commitment is set, that template; all zeros disables it (`B_PRE > 0` is required, no reliance on
the hash comparison). The leader path, slot limits, state layout, entrypoints and dispatch tags are the reference's.
The leader is `KCC20Batch` (16 slots, amount 0, conservation and the KCC-20 per-output rules, each holder still
authorizes its own owner), unchanged from proposal P2.

Templates. The reference has no constructor constants, so every reference token shares one template. `KCC20Opt` has one
template per commitment value: one shared by every token without a leader (all zeros) and one shared by every token
that commits to `KCC20Batch`, because `KCC20Batch` keeps the holder template it serves, the owner and the
extension commitment in its state, so its own template does not depend on the token. With the enabled program a token
has a leader only if its genesis creates a seed: no holder or leader path can create one later. Where the commitment
lives, and why not elsewhere:

| Option | Effect |
|---|---|
| Constructor constant (chosen) | Two templates for the whole ecosystem (disabled, universal leader); a per-token leader of another program gives that token its own template |
| `KCC20State` field | Breaks the fixed 112-byte state (section 1): the `transfer` dispatch tag (`State[]` field list), every `next_states` encoding, the conformance vectors, and every program and tool that reads token states at fixed offsets (KOB orders, `KCC20Batch`, indexers, wallets); every state pushed in a transaction grows by 33 bytes |
| Inside `extension_commitment` | Gives one template, but the field is the fungibility class of an application's extended state (section 4): a token could not have both, and a hashed opening would need a witness the delegator entrypoint does not take |

Measured (`kcc20_opt_tests.rs`, `opt_measurements`; P2SH sizes, fee = max(compute mass, 2 x bytes) x 100 sompi):

| Program | Holder | 1 -> 1 | 3 -> 1 | 8-ask sweep | 16-ask sweep | 8 x 8 cross |
|---|---|---|---|---|---|---|
| Reference 3/3 | 3,090 B | 0.00702 | 0.01990 | n/a | n/a | n/a |
| Reference 8/8 (KOB issue) | 6,820 B | 0.01448 | 0.04228 | 0.14060 | n/a | n/a |
| Reference 16/16 | 12,788 B | 0.02642 | 0.07808 | 0.23608 | 0.47071 | n/a |
| P2 | 3,095 B | 0.00703 | 0.01993 | 0.09341 | 0.17295 | 0.12110 |
| `KCC20Opt` disabled | 3,105 B | 0.00705 | 0.01999 | refused | refused | refused |
| `KCC20Opt` enabled | 3,109 B | 0.00706 | 0.02001 | 0.09364 | 0.17340 | 0.12133 |

Both instances run the KCC-20 conformance vectors in `kcc20_conformance_tests.rs` (`KCC20Opt-off`, `KCC20Opt-on`) with
the same results as the 3/3 reference.

Observation for upstream: the reference delegator does not check its leader. KCC-20 section 2 says a delegator
validates only its local owner, and the reference's read of the leader state is unused (it only requires the leader's
signature script to be at least as long as the holder program). Any program that a token's genesis puts in its
covenant family can therefore lead reference holders: `KCC20Batch` works with unchanged 3/3 holders (the cheapest
row, measured as `KCC20Ref 3/3 + lineage leader`: 0.17279 KAS for the 16-ask sweep), and so would a script that does not
validate the transition (`reference_delegator_accepts_any_lineage_leader`). The reference's safety rests on the
genesis (KCC-1 section 7.2); `KCC20Opt` makes the holders name the leaders they accept.
