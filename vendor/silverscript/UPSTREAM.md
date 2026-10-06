# Vendored SilverScript

| | |
|---|---|
| Upstream | SilverScript (Kaspa developers), ISC license, see `LICENSE` and `CREDITS.md` |
| Version | v1.0.0 |
| Commit | `3ed973335b59269293564805cc2c58a14595ec03` ("Prepare SilverScript 1.0 and clarify ABI artifact validation (#248)") |
| Crates kept | `silverscript-lang` (`src/`, `std/`, `Cargo.toml`), `silverscript-abi` |
| Dropped | tests, debugger crates, debug-artifact crate, editor extensions, tree-sitter grammar, docs |

The first commit that added this directory is byte-identical to the upstream blobs at the
commit above (line endings LF). Everything below was changed afterwards and is visible as a
normal git diff of `vendor/silverscript`.

## Why the fork exists

Upstream v1.0.0 pins rusty-kaspa to git revision `a41a333b`. KOB targets the Toccata
mainnet node v2.1.0 (tx v1, covenants, compute budget), and upstream v1.0.0 does not build
against it (upstream issue #256). Until that is fixed upstream, KOB keeps this small fork and
uses a single rusty-kaspa version across the whole workspace: `tag = "v2.1.0"` (see the root
`Cargo.toml`). This directory can be dropped, and the root `silverscript-*` dependencies
switched to upstream, once upstream releases a version that builds against rusty-kaspa v2.1.0.

## Changes

1. **rusty-kaspa v2.1.0 API renames** (`patches/0001-rusty-kaspa-v2.1.0-api.patch`, 7 files,
   +20/-20 lines, source only):
   - `max_ops_per_script(true)`, `max_scripts_size(true)`, `max_script_element_size(true)`
     became the constants `MAX_OPS_PER_SCRIPT`, `MAX_SCRIPTS_SIZE`, `MAX_SCRIPT_ELEMENT_SIZE`.
   - `EngineFlags { covenants_enabled }` was removed; covenants are always on, so
     `ScriptBuilder::with_flags(..)` became `ScriptBuilder::new()` and `EngineFlags::default()`.
   - `deserialize_i64(bytes, bool)` lost its second argument.
   - `MAINNET_PARAMS.new_max_signature_script_len` became `max_signature_script_len`.
   - The patch was derived from the working v2.1.0 port of upstream v1.0.0; only the hunks
     under `silverscript-lang/src` and `silverscript-abi/src` are kept. Patch hunks for tests
     and the debugger are not vendored. One import was tidied and the result rustfmt-formatted.
2. **Cargo wiring.** The crates now live in the KOB workspace: dependencies on rusty-kaspa and
   shared crates come from the root `[workspace.dependencies]`; package metadata (version,
   edition 2024, license, rust-version 1.94.0) is written out explicitly because the KOB
   crates use edition 2021. Dev-dependencies that only served the dropped `tests/` directory
   (`kaspa-consensus`, `kaspa-addresses`, `kaspa-muhash`, `borsh`, `risc0-zkvm`, `sha2`,
   `tokio`) were removed.
3. **wasm32 randomness.** Both crates declare, under
   `[target.wasm32-unknown-unknown.dependencies]`, the `getrandom` 0.3 (`wasm_js`) and 0.2
   (`js`) backends. getrandom 0.3 additionally needs the cfg flag
   `--cfg getrandom_backend="wasm_js"`, set once for the whole workspace in
   `.cargo/config.toml`. With this, `silverscript-lang`, `silverscript-abi` and `kob-wasm`
   build for `wasm32-unknown-unknown`. This block is a KOB addition, not part of upstream.

## Updating

Re-import the two crates from a newer upstream commit, re-apply the changes above (or drop the
ones upstream already contains), then run `cargo test --workspace` and
`scripts/build-contracts.sh --check`; the contract artifacts must stay byte-identical unless
the change is intentional.
