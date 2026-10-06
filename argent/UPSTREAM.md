# Argent upstream

KOB's order contracts are hand-written SilverScript; Argent is used to publish them to other apps
(`docs/argent.md`) and to write KOB's own composition layer, the router. This directory pins the
Argent compiler (`argentc`) used for that, holds our two patches to it and builds it reproducibly.

| | |
|---|---|
| Upstream | [argent-lang/argent](https://github.com/argent-lang/argent), ISC license, pre-1.0 |
| Pinned commit | `b312deda6fe10f6493c8d49eb748e3f61860458a` ("Refresh README status and runtime example", 2026-09-30: master after the module loading rework, #62) |
| SilverScript | v1.0.0 (`3ed97333`), which upstream pins since #60; built from `vendor/silverscript` (patch 0001) |
| Pin file | `upstream.lock` (url, rev), read by `build-argentc.sh` |
| Patches | `patches/*.patch`, applied in name order on top of the pin |
| Checkout | `argent/upstream/` (git-ignored; created and reset to the pin by the build script) |
| Build output | `target/argent/release/argentc` (`ARGENT_TARGET_DIR` overrides) |

## Build

```
argent/build-argentc.sh              # clone, pin, patch, build; prints the path of argentc
argent/build-argentc.sh --verify     # also check vendor/argent-artifact against the pinned crate
ARGENT_URL=<path or url> ...         # clone source override (the pin is still enforced)
```

The script uses the toolchain of `rust-toolchain.toml` (1.94.0), builds with `--locked`, and only
resets and re-patches the checkout when the pin or a patch changed (the applied state is
stamped in `argent/upstream/.git/kob-stamp`), so an unchanged tree does not rebuild. Delete
`argent/upstream/` to force a fresh clone. `scripts/build-argent.sh` calls it; that is the
entry point CI uses (`scripts/build-contracts.sh --check`).

## Patches

Both patches are needed at `b312ded`:

1. `0001-argent-rusty-kaspa-v2.1.0.patch` (Argent issue #64, "tbd" upstream). Upstream still builds
   against rusty-kaspa `a41a333b`; Toccata mainnet runs node v2.1.0. The patch moves the rusty-kaspa
   dependencies to `tag = "v2.1.0"` (root `Cargo.toml` and `Cargo.lock`), points
   `silverscript-abi`/`silverscript-lang` at `vendor/silverscript` (KOB's v2.1.0 port of
   SilverScript v1.0.0, `../../vendor/silverscript` from the checkout), and adapts two lines of
   `argent-runtime` to the v2.1.0 API (`MAINNET_PARAMS.block_mass_limits`, covenants always
   enabled). Same family as `vendor/silverscript/patches/0001`. Drop it when upstream builds against v2.1.0.
2. `0002-argent-artifact-backed-app-imports.patch` (parser, loader, bundle builder; tests and docs).
   Written against the module loading rework (#62), where imports are plain module imports
   (`import "<path>" [as <alias>];`; `App::Actor` names an app member). The patch adds an optional pin
   to a module import,
   `import "./x/artifact.json" id "<artifact id>";`: a `.json` import loads a published app artifact,
   runs `check_consistency()` on it, requires its id to be the pinned one, and turns its states,
   actor enums, exported actors and app into a declaration-only module; the bundle builder links the
   artifact (interface fingerprints, actor-type handles as constants) and never compiles it. An
   artifact import without `id`, or with another id, is refused: `check_consistency()` only proves
   the artifact is self-consistent (its id is its content hash), and a forged artifact with the same
   app name and ABI is self-consistent too; `id` on a source or standard-module import is refused. This is closed ICC for apps whose source is not Argent source,
   which is what KOBOrders is (hand-written SilverScript). Artifact-backed dependencies are still
   Argent's own open item ("Support artifact-backed app dependencies",
   `docs/app-linking-followups.md`). The patch is byte for byte the branch prepared as an upstream
   PR (two commits on `b312ded`, with upstream-style tests and docs). Open ICC (`actor_type<State>`
   handles) needs no patch.

Evidence: the v2 contracts, the KOBOrders artifact and the router
(`crates/kob-tests/tests/argent_router_tests.rs`, `argent_import_pin_tests.rs`).

## Vendored crate

`vendor/argent-artifact` is `crates/argent-artifact` of the pinned commit (`src/` byte for
byte; only `Cargo.toml` is rewritten to use the KOB workspace). It is the artifact data model
(`Artifact`, template receipts, handles, fingerprints, ids) that `tools/sil2argent` builds on
and that the workspace tests use, so `cargo test --workspace` does not need the Argent
checkout. `build-argentc.sh --verify` fails if it drifts from the pin.

## Updating the pin

1. Set `rev` in `upstream.lock`; delete `argent/upstream/`.
2. Re-create the patches against the new commit (`git -C argent/upstream diff`), keep the
   numbering.
3. Re-import `crates/argent-artifact/src` into `vendor/argent-artifact/src` and update the commit in
   `vendor/argent-artifact/UPSTREAM.md`.
4. `scripts/build-contracts.sh` and review the diff of `contracts/argent/**`: the KOBOrders
   handles must not change (they are the hand-written template hashes); the artifact ids may (for
   example when argentc lists the states of an imported module in another order).
5. If the router changed (its artifact id or a template hash): update `ROUTER_ARTIFACT_ID` and
   `ROUTER_PINNED` in `crates/kob-protocol/src/router.rs`, `ROUTER_ARTIFACT_ID` in
   `packages/kob-x402/src/types.ts` and the id in `docs/argent.md`, and refresh the embedded ABI
   (`KOB_WRITE_ROUTER_ABI=1 cargo test -p kob-protocol --lib embedded_abi_is_the_committed_artifact`);
   the web build embeds it through `kob-wasm`.
6. `cargo test --workspace`.
