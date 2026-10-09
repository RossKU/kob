# KCC base-spec conformance (KCC-1, KCC-2, KCC-20)

Status: checked on 2026-10-02 against the Last Call texts; KCC-20 and its reference program re-checked on 2026-10-09,
after both were merged upstream (sections 2 and 4). KOB handles any token through adapters, but its own KCC-20
programs, its issuance and every encoding it labels KCC must conform to the KCC base specifications. This file records
what was checked, against which upstream bytes, what did not conform and what changed, and what still depends on
upstream.

## 1. Pins

| Spec | Upstream (kaspanet/kccs) | Status there | Vectors in this repo |
|---|---|---|---|
| KCC-1 Covenant Concepts, Byte Layouts, and ABI | `main` `411b41b` (compliance rewrite #27, Last Call #32) | Last Call | `crates/kob-tests/vectors/kcc1/conformance.json` |
| KCC-2 Authority Schemes | `main` `411b41b` (#30: unkeyed P2PKH hash, scheme ranges; Last Call #33) | Last Call | `crates/kob-tests/vectors/kcc2/authority-schemes.json` |
| KCC-20 Fungible Token | `main` `3fbec52` (#31 merged 2026-10-07) | Last Call | `crates/kob-tests/vectors/kcc20/conformance.json` |
| KCC-20 reference program | argent-lang/kcc20-reference `master` `c8a0871` (PR #1 merged 2026-10-07; argentc `9a9f4b1`) | merged | `contracts/third-party/kcc20-reference/` (verbatim), built as `contracts/kcc20/KCC20Ref.sil` |

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

Re-vendored on 2026-10-09 from `main` `3fbec52` (PR #31 merged; the same bytes as the reference repository's own
snapshot). The only change from `cfb74cf`: seven amount-threshold borrowed-receive cases (zero, negative and negative-zero
thresholds, unchanged and smaller successors, a normalized guard), which apply to programs that permit negative threshold
payloads (KCC-20 section 5: a negative threshold behaves as zero, a program MAY require it to be non-negative). The merged
reference permits them, so every program runs all 17 cases: `KCC20Ref`, `KCC20Ref_8x8`, `KCC20Opt-off`, `KCC20Opt-on` pass
every section (state, dispatch, standard transfer, owner witness for schemes `0x00`-`0x04`, borrow witness, hash chain,
borrowed receive). The harness funds a successor gain from a delegator as before; an unchanged or smaller successor
conserves the amount on its own (the difference goes to a second output), so the borrow rule alone decides.

## 3. Mismatches found

| # | Where | What | Impact | Fix |
|---|---|---|---|---|
| a | `kob-x402` `token::leader_next_states` | Decoded the `State[]` amounts as two's complement (KCC-1 `int` is 8-byte signed magnitude) and gave up when a one-element field group was pushed as `OP_1`..`OP_16` (the KCC-1 `PushMinimal` form of a single successor with `owner_scheme` `0x01`-`0x04` or `borrow_scheme` `0x01`-`0x03`) | Diagnostics only (acceptance is by recomputation): a less precise rejection reason | Decodes with the strict KCC-1 decoder (`kob_protocol::kcc20::transfer_next_states`) |
| b | `kob-executor` indexer `kcc20_leader_next_states` | Same two's-complement amount decoding | None in practice (accepted transactions only, the programs require amounts >= 0) | Same shared decoder |
| c | Router (`crates/kob-protocol/data/router_sil_abi.json`) | The Argent router's intent states declare `deadline` as SilverScript `temporal`, which is not a KCC-1 type name | State bytes are identical to KCC-1 `int`; it is a state field only (no entrypoint, so no dispatch tag) | Recorded and allowlisted in the sweep; the type comes from SilverScript/Argent upstream |
| d | Router `IntentState::decode` | Used silverscript-abi's state decoder alone, which accepts non-canonical bytes (negative zero, an over-long push prefix); KCC-1 3.7.1 requires the consumed bytes to equal the canonical encoding | A malformed intent state could decode | Re-encodes and compares (as the order and token state codecs already did) |
| e | Issuance (`issue.rs`) | Owner schemes checked as `<= 0x04`; correct, but not tied to the registry | None | Constants are the `kcc2` bytes; the refusal names the KCC-2 range (reserved or custom) |

In that review (2026-10-02) no KOB-owned program, template hash, registry entry, Argent handle, budget, tip, vector or
deploy file changed: everything KOB encodes or executes already conformed. The 2026-10-09 change of the token programs
comes from the merged reference (section 4.2), not from a mismatch.

Upstream observations (not KOB code): the vendored SilverScript compiler rejects the three KCC-1 invalid argument types
and the truncated dispatch-tag collision; silverscript-abi's state decoder alone accepts negative zero and over-long
push prefixes, so every KOB caller re-encodes to check (`kcc1::decode_state`, `StateCodec`, the router).

## 4. The token programs

### 4.1 Unkeyed P2PKH hash

KCC-2 #30 replaced the keyed `Hash(pubkey, "PublicKeyHash")` with the unkeyed `Hash(pubkey)`. The reference program
(unmerged `600646873e` and merged `c8a0871` alike) computes `p2pkh_hash` as `blake3(public_key)`, which compiles to
`OP_BLAKE3`, the unkeyed hash; KOB's copies derive from it (`KCC20Ref.sil`; the slot variants, `KCC20P2`, `KCC20Opt`,
`KOBToken` and the wallet-gate copy). `crates/kob-tests/tests/kcc2_conformance_tests.rs`
(`kcc20_programs_use_the_unkeyed_p2pkh_hash`) tracks this: it fails if a keyed form returns or the provenance commit moves.

### 4.2 The merged reference (2026-10-09)

argent-lang/kcc20-reference PR #1 was merged on 2026-10-07 as `c8a0871`. KOB had vendored the program of the unmerged head
`600646873e`. The merged `contracts/kcc20.ag` differs in three places, and the repository no longer declares a
single-actor app for it: it publishes the token only as the `KCC20` actor of its `KCC20PublicMint` app
(`contracts/third-party/kcc20-reference/UPSTREAM.md` compares the two builds).

Behaviour, old program to merged program (the standalone build KOB uses):

| # | Where | Old (`600646873e`) | Merged (`c8a0871`) | Effect |
|---|---|---|---|---|
| 1 | Successor `owner_scheme` / `borrow_scheme` check | five / four equality tests | `unsigned(x) <= 0x04` / `<= 0x03` | none: the same bytes are accepted (all 256 checked in `kcc2_conformance_tests.rs`); smaller program |
| 2 | Successor with `borrow_scheme` `0x01` | refused if its threshold (`borrow_guard[0..8]`, signed magnitude) is negative | accepted with any threshold | a transfer may create a state whose threshold is negative |
| 3 | Borrowed receive under `amount-threshold/v1` | refused if the threshold is negative | a negative threshold acts as zero (`amount_increase > max(threshold, 0)`); the guard must still be preserved | such a state can be borrowed by adding at least one unit (vectors `threshold-negative*`) |

Unchanged: the 112-byte state at offset 1 and its field order, the constructor, the entries, their parameter types and
dispatch tags (`transfer` `79c71c23`, `transfer_delegator` `fd3ef14a`), the limits (3 token inputs, 3 token outputs),
conservation, the owner schemes and witnesses, the other borrow schemes, `readInputState` for delegates and leader (no
template check), the continuation outputs. The standalone build is reproducible: argentc `9a9f4b1` (the compiler the
reference pins) and KOB's pinned argentc (`b312ded` + patches) give the same SilverScript, and `scripts/build-argent.sh`
rebuilds `KCC20Ref.sil` from the vendored `kcc20.ag`.

The published `KCC20PublicMint` build of the same actor differs from the standalone build by compiler-owned context, not by
token rules: state 1 + 145 bytes (`gen__kcc20_template`, the template hash, before the six fields; constructor likewise),
the template lengths as fixed-width constants (argent #68), delegates and leader read with `readInputStateWithTemplate`
(so every holder checks that its leader is a `KCC20`), the template copied into every successor state; 4,031 B, the same
dispatch tags and limits. KOB's orders, router, indexer, x402 profile and wallets read the token state as the 1 + 112 layout
at fixed offsets, so adopting that build would change all of them; KOB keeps the standalone build.

Template hashes (all `prefix 1`, state 112):

| Program | Old template, size | New template, size |
|---|---|---|
| `KCC20Ref` (3/3) | `f4ac029d...`, 3,090 B | `173ca6a796c2c05f171c31b9a73aaca161a3226a9e8f57d2f3d833ff18dbe41b`, 2,915 B |
| `KCC20Ref_4x5` | `6bef6739...`, 4,182 B | `10d3d2ff9efacb64e2ba3acb3e442fb2a35128b36a9ad83fb99b7fe053505983`, 3,889 B |
| `KCC20Ref_8x8` (KOB issue, `KOBToken::KCC20`) | `40fef59a...`, 6,820 B | `666da060d02663939efdc534ea10cce219e5564af86f4cc8eeea2ca129f7c032`, 6,350 B |
| `KCC20Ref_16x16` | `8319cdc4...`, 12,788 B | `922e9ba7c64b0ddcbd0f1b4b6bd592c791e0813df033d1c78213e4eec374efd7`, 11,846 B |
| `KCC20P2` | `16032d0d...`, 3,095 B | `b182879fe8d87659f9424dbdedc6540fba185129c3d7dd5e9caa1332f0ce4d11`, 2,920 B |
| `KCC20Opt` (disabled instance) | `77e25134...`, 3,105 B | `1de86404b89e381471e7ff2088b41f097e47c4d746a91300d9c8322a52654619`, 2,930 B |

`KCC20Batch` is unchanged (`98caa910...`: it names the holder template in its state). The `KOBToken` artifact id is now
`364f6518...` (`docs/argent.md`). Regenerated with the repository's generators: the slot variants, `KCC20P2` and `KCC20Opt`
(`KOB_REGEN=1` on their generator tests), `KOBToken` and the artifacts (`scripts/build-contracts.sh`), the wallet-gate copy and
its artifacts, the compute-budget table, the keeper tips, the protocol and x402 golden vectors, the mainnet deployment
record (it pins the registry); the registry, the Rust / TS pins and the web and soak fixtures name the new templates.
The order templates do not change (they take the token template as a value).

Tokens issued under the old `KCC20Ref_8x8` keep their program on chain; this build pins only the new template, so it no
longer lists, builds for or matches them (as for any template it does not pin: [template-retirement.md](template-retirement.md)),
and orders whose `tokenTplHash` names the old template are unknown to it. On testnet the tokens are issued again under the
new program.

## 5. Pending on upstream

| Item | Current upstream | Expected change | Effect on KOB |
|---|---|---|---|
| KCC-20 notation | The merged text cites KCC-1 3.8.1 and 3.9 but still writes `P2PKHHash(pubkey)` where KCC-2 says `Hash(pubkey)` | Notation | None on bytes: its vector values for `P2PKHHash` are the unkeyed BLAKE3 (`7caa514a...` = `Hash(22^32)`), which the KCC-20 test asserts |
| KCC-20 record name | Default configuration names the record `KCC20State`; the standalone build's artifacts name it `State` (the public-mint build: `KCC20State` parameter, `State` runtime record) | Possibly a name alignment | None: dispatch tags omit record names (`transfer` `79c71c23`, `transfer_delegator` `fd3ef14a`) |
| KCC-20 Final | Last Call on `main` `3fbec52` | Final, possibly new vector bytes | Re-vendor `vectors/kcc20/` (the sha256 pin fails on any change) and rerun |
| Reference program | kcc20-reference `master` `c8a0871`, published only as the `KCC20PublicMint` build | A new commit, or a published single-actor build | Rerun section 4; if the standalone bytes change, regenerate the pinned artifacts (orders under the old template become unsupported: [template-retirement.md](template-retirement.md)) |

Observation for upstream: the `covenant-id/v1` minimum check also passes when `owner` is the token's own covenant id
(`covenant-self` is approved, and KOB's programs approve it as the vector requires), so such a state can be moved by any
transaction that spends it. KCC-2 section 6 warns that participation alone is not approval; KCC-20 PR #31 does not
forbid this owner. KOB issuance cannot create it (the covenant id is derived from the genesis outputs).

## 6. Optional batch leader (proposal for upstream, not issued)

The default configuration compiles `max_token_inputs` / `max_token_outputs` = 3 into the holder program, and every token
input pushes the whole program, so larger limits make every transfer dearer (1 -> 1 payment: 3/3 0.00667 KAS, 8/8
0.01354, 16/16 0.02454). `contracts/kcc20/p2/KCC20Opt.sil` (generated from the reference by
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
| Reference 3/3 | 2,915 B | 0.00667 | 0.01885 | n/a | n/a | n/a |
| Reference 8/8 (KOB issue) | 6,350 B | 0.01354 | 0.03946 | 0.13308 | n/a | n/a |
| Reference 16/16 | 11,846 B | 0.02454 | 0.07243 | 0.22101 | 0.44056 | n/a |
| P2 | 2,920 B | 0.00668 | 0.01888 | 0.09061 | 0.16735 | 0.11830 |
| `KCC20Opt` disabled | 2,930 B | 0.00670 | 0.01894 | refused | refused | refused |
| `KCC20Opt` enabled | 2,934 B | 0.00671 | 0.01896 | 0.09084 | 0.16780 | 0.11853 |

(Re-measured on the merged reference, 2026-10-09. On the unmerged one the programs were 175 B (3/3, P2, `KCC20Opt`) to
942 B (16/16) larger and the fees 3 to 7 % higher: 3/3 1 -> 1 0.00702, 8/8 1 -> 1 0.01448, 16/16 16-ask sweep 0.47071, P2
16-ask sweep 0.17295, `KCC20Opt` enabled 0.17340. `KCC20P2` and `KCC20Opt` are generated from the merged standalone
reference: non-standard holder variants, outside the default configuration.)

Both instances run the KCC-20 conformance vectors in `kcc20_conformance_tests.rs` (`KCC20Opt-off`, `KCC20Opt-on`) with
the same results as the 3/3 reference.

Leaders. The standalone reference delegator does not check its leader: its read of the leader state is unused (it only
requires the leader's signature script to be at least as long as the holder program). Any program that a token's genesis
puts in its covenant family can therefore lead standalone holders: `KCC20Batch` works with unchanged 3/3 holders (the
cheapest row, measured as `KCC20Ref 3/3 + lineage leader`: 0.16719 KAS for the 16-ask sweep), and so would a script that
does not validate the transition (`reference_delegator_accepts_any_lineage_leader`). The reference's safety there rests on
the genesis (KCC-1 section 7.2). The merged reference's published build (`KCC20PublicMint`) closes this: every holder
reads its leader with `readInputStateWithTemplate`, so only a `KCC20` can lead, which is what `KCC20Opt` disabled does; and
a `KCC20` leader takes at most 2 delegates and 3 outputs.

Transactions per holder count (`crates/kob-tests/tests/kcc20_reference_tests.rs`, `transfer_capacity_per_transaction`):
with the standard program a transaction carries at most 3 token inputs and 3 token outputs of one token (one covenant
family, one leader). Holders that stay on the standard 3/3 program cannot be settled 16 at a time: in the published build
every holder refuses a foreign leader (16 of 16 refused), and in the standalone build only a leader seeded by the token's
genesis could do it, which KOB's issuance never creates. A 16-holder transaction needs a non-default program for the holders:
the 16/16 slot variant (11,846 B pushed by every token input) or a holder program that names a batch leader (`KCC20P2`,
`KCC20Opt` enabled; 2,920 / 2,934 B), both outside the default KCC-20 configuration.
