# Vendored argent-artifact

| | |
|---|---|
| Upstream | argent-lang/argent, crate `crates/argent-artifact`, ISC license (`LICENSE`) |
| Commit | `b312deda6fe10f6493c8d49eb748e3f61860458a` (the pin in `vendor/argent-upstream/upstream.lock`; `src/` is the same as at the earlier pin `e76ee07`) |
| Kept | `src/` byte for byte |
| Changed | `.rustfmt.toml` (upstream copy, so `cargo fmt --all --check` accepts `src/` as is); `Cargo.toml`: package metadata written out (upstream inherits it from the Argent workspace), dependencies from the KOB workspace, `thiserror = "2"` as upstream |

Why: `tools/sil2argent` and the harness read and verify Argent artifacts. Vendoring the data
model keeps `cargo test --workspace` independent of the Argent checkout and of argentc.
`vendor/argent-upstream/build-argentc.sh --verify` (part of `scripts/build-contracts.sh --check`) fails when
`src/` differs from the pinned upstream crate. See `vendor/argent-upstream/UPSTREAM.md` for updating.
