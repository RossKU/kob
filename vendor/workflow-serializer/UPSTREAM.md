# Vendored workflow-serializer

| | |
|---|---|
| Upstream | `workflow-serializer` 0.18.0 on crates.io (workflow-rs/workflow-rs), `MIT OR Apache-2.0`, used under MIT (`LICENSE-MIT`) |
| Checksum | `64679db6856852a472caff4ce869e3ecebe291fbccc9406e9643eb5951a0904a` (the `.crate` file, as in the `Cargo.lock` before this copy) |
| Kept | `src/` byte for byte, except the patch below |
| Changed | `Cargo.toml`: the published manifest written out without the generated header, `publish = false`; `rustfmt.toml` (rustfmt defaults, so `cargo fmt --all --check` accepts `src/` as published) |

Why: kobd decodes the Borsh wRPC answers of nodes it does not control (public nodes, failover
nodes) with the node's own types from `kaspa-rpc-core`, which read their vectors through this
crate. Upstream `impl Deserializer for Vec<V>` calls `Vec::with_capacity(len)` with a `u32`
length read from the input. An answer of about 35 bytes that claims 0xFFFFFFFF chain blocks
asks for terabytes, and a failed allocation aborts the process; it is not a panic that can be
caught. Any Borsh peer could stop an Executor or an Indexer this way. The length fields sit at
positions that depend on everything before them in the answer, so kobd cannot bound them before
decoding without re-implementing the whole layout.

## The patch

`src/serializer.rs`, `impl Deserializer for Vec<V>`: the capacity reserved up front is at most
4 KiB worth of items (`cautious`), as `borsh` does for its own vectors. The vector grows with the
items actually read, so a length the input does not back ends in an `UnexpectedEof` error. The
decoded values are the same as upstream's for every input upstream decodes.

The root `Cargo.toml` puts this copy in place of the crates.io crate with `[patch.crates-io]`,
for every crate of the workspace that uses it (`kaspa-rpc-core`, `kobd`, `kob-cli`). The test
`rpc::borsh::tests::a_huge_length_prefix_is_an_error_not_an_abort` in kobd covers it.

## Updating or dropping

When `kaspa-rpc-core` moves to another `workflow-serializer`, compare this copy with the new
upstream `src/`, and drop it (with the `[patch.crates-io]` entry) once upstream bounds its
pre-allocation.
