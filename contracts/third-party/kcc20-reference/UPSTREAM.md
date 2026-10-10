# KCC-20 reference program (third-party, vendored unmodified)

| | |
|---|---|
| Upstream | https://github.com/argent-lang/kcc20-reference |
| Commit | `c8a087117735a1f87c5c6d115fcddeaf2562c784` (`master`, "Add KCC20 reference, public mint app, and offline examples (#1)", 2026-10-07: pull request #1 squash-merged) |
| Compiler pinned there | argentc `9a9f4b107116d0b259fae629f200fa3b663e7e9d` (argent-lang/argent, "Embed current-actor template lengths as fixed-width constants (#68)"; `Cargo.toml` `argent` / `argent-runtime`) |
| Licence | ISC, as declared in that repository's `Cargo.toml` (no LICENSE file at that commit) |
| Status | Not audited. Implements KCC-20 as merged on kaspanet/kccs `main` `3fbec52` (Last Call) |

| File | Upstream path | Git blob | sha256 |
|---|---|---|---|
| `kcc20.ag` | `contracts/kcc20.ag` | `581c9b76591c23f45faaf3ff473659762b15b1e3` | `f6131d97368461e74b02507d5ef2d7af57c76226cf945ebbbe9735f0953414e9` (7,497 B) |
| `KCC20.public-mint.sil` | `fixtures/public-mint/sil/KCC20.sil` | `cab6cc21f1ea3d1e0be1caa0907f3996bcd0ea04` | `9c61d62a1b90ad541bbf5c48ec53faaff1826cbc6f26bd42fa429df5c295316c` (12,136 B) |
| `public-mint.artifact.json` | `fixtures/public-mint/artifact.json` | `30a4a75b45bc75764e55702e6486987667ae1764` | `a419f2aa5f18f2917bf1cd69fbf04942ef746fc832f8e1fcd7625582d4c6c771` (110,136 B) |

The three files are byte for byte the upstream blobs; `crates/kob-tests/tests/kcc20_reference_tests.rs` pins the sha256
values. The fixtures are renamed so that they are not mistaken for KOB files. `KCC20.public-mint.ctor.json` is KOB's
(the constructor arguments `scripts/build-contracts.sh` compiles the fixture with: `gen__kcc20_template` = the program's
own Sil template hash, then the example state of `contracts/kcc20/KCC20Ref.ctor.json`); the build writes
`contracts/artifacts/KCC20PublicMint.json` from the unmodified fixture, the only file of `contracts/third-party/` it
compiles.

## Two builds of one actor

Upstream's `kcc20.ag` declares the `KCC20` actor and, since the merge, no app. The repository publishes it only as
one of three actors of its `KCC20PublicMint` app (`contracts/public_mint.ag`: `KCC20`, `PublicMint`, `TokenSeed`),
whose generated program is the fixture `KCC20.public-mint.sil`. `cargo run --locked --example build_contracts` at
that commit (toolchain 1.94.1) reproduces the fixture byte for byte.

| | Standalone (`contracts/kcc20/KCC20Ref.sil`) | Inside `KCC20PublicMint` (`KCC20.public-mint.sil`) |
|---|---|---|
| Build | `kcc20.ag` + `app KCC20Reference { actor KCC20; }`, argentc `9a9f4b1` (KOB's pinned argentc gives the same output) | `public_mint.ag`, argentc `9a9f4b1` |
| Program | 2,915 B | 4,031 B |
| State | offset 1, 112 B: `amount`, `owner`, `owner_scheme`, `borrow_scheme`, `borrow_guard`, `extension_commitment` | offset 1, 145 B: `gen__kcc20_template` (32-byte template hash, compiler-owned) then the same six fields |
| Constructor | the six state values | `gen__kcc20_template` first, then the six state values |
| Template hash | `173ca6a796c2c05f171c31b9a73aaca161a3226a9e8f57d2f3d833ff18dbe41b` | `9703112ee6e3555107cd168858992b77d3b74f655205b2b463b1f9ec2ec73cf7` (prefix 1 B, suffix 3,885 B) |
| Delegates read by the leader | `readInputState` (no template check) | `readInputStateWithTemplate(idx, prefix_len, suffix_len, gen__kcc20_template)` |
| Leader read by a delegator | `readInputState`, value unused (no template check) | `readInputStateWithTemplate(...)`: the leader must be a `KCC20` |
| Continuations | `validateOutputState(next_states[i])` | the same, with `gen__kcc20_template` copied into every successor state |
| Dispatch tags | `transfer` `79c71c23`, `transfer_delegator` `fd3ef14a` | the same |
| Limits | 3 token inputs, 3 token outputs | the same |

The template context is what argentc adds to an actor that shares its covenant with other actors (the public-mint
app's minter and seed live in the token's covenant family); `kcc20.ag` itself contains no template check.

## Both builds on KOB

KOB issues with the standalone 3 / 3 build by default (`kob token issue`, the web app's default program; KOB's standard
token program) and offers the published build as the second choice (`kob token issue --program public-mint`, the web
app's second option); it trades tokens of both builds. The context field is constant: in
every UTXO of the published build it is the program's own Sil template hash (`9703112e...`), and the 112 bytes after
it are the standalone state byte for byte. So KOB pins the published build as its KCC-1 actor-type handle, the view
upstream's `public-mint.artifact.json` exports for `actor_type<KCC20State>` (`template/kcc20`): prefix = the 1-byte
Sil prefix `6b` plus the context push `20 9703112e...` (34 B), open state = the 112-byte `KCC20State`, suffix = the
Sil suffix (3,885 B), template hash `734850b0af0aeef49f009167bf9ddd238fd8c5cdbe214234f97181ac6f5cc498`
(`TemplateId::Kcc20PublicMint`, registry `kcc20-ref-public-mint`). The order contracts take the token template per order
(`tokenTplHash`, `tplPrefixLen`, `tplSuffixLen`) and read the state at fixed offsets inside the 112 bytes, so they custody,
fill and rebuild such tokens unchanged; a holder with any other context value is another template to them. Evidence:
`crates/kob-tests/tests/kcc20_public_mint_tests.rs` and `registry_tests.rs` (the handle cut equals upstream's).
`kob token issue --program public-mint` issues a fixed-supply token of the published build (a genesis of `KCC20`
holders, no `PublicMint`, no `TokenSeed`).

A token issued the way upstream recommends (a genesis of one `PublicMint` and `TokenSeed` seeds, with or without
pre-minted holders) is verified too: `kob registry verify-genesis` reads the app's `PublicMint` and `TokenSeed` programs
from `public-mint.artifact.json` (`crates/kob-protocol/src/public_mint.rs`, Sil template hashes
`14b0f06bc176b78c94f19292e409c622636f31dae6bb31ff3d4d3308a6053b49` and
`f79a3fccf6f87d7076662410a17e54d1531889d9b60f630f9fbb8465a384e62d`), requires their template fields to name the app's
own programs and one extension commitment, and reports the minters' allowance: the maximum supply is the genesis holders'
amounts plus that allowance, fixed at genesis. Rules and evidence: `docs/spec/kcc-conformance.md` section 4.3,
`crates/kob-tests/tests/kcc20_public_mint_genesis_tests.rs`.
