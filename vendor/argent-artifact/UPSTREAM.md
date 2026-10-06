# Vendored argent-artifact

| | |
|---|---|
| Upstream | argent-lang/argent, crate `crates/argent-artifact`, ISC license (`LICENSE`) |
| Commit | `e76ee07f8b2719e8c06eee085ca3d613cc2b56e7` (the pin in `argent/upstream.lock`) |
| Kept | `src/` byte for byte |
| Changed | `.rustfmt.toml` (upstream copy, so `cargo fmt --all --check` accepts `src/` as is); `Cargo.toml`: package metadata written out (upstream inherits it from the Argent workspace), dependencies from the KOB workspace, `thiserror = "2"` as upstream |

Why: `tools/sil2argent` and the harness read and verify Argent artifacts. Vendoring the data
model keeps `cargo test --workspace` independent of the Argent checkout and of argentc.
`argent/build-argentc.sh --verify` (part of `scripts/build-contracts.sh --check`) fails when
`src/` differs from the pinned upstream crate. See `argent/UPSTREAM.md` for updating.
