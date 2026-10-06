# KaspaCom KCC20 token program (third-party, vendored unmodified)

| | |
|---|---|
| What | `KCC20.placeholder.json`: the compiled KCC20 token program artifact (SilverScript ABI artifact, 25,552-byte program, 112-byte KCC-20 state at offset 1) |
| Upstream | https://github.com/KASPACOM/kcc20-tx-builder, `artifacts/KCC20.placeholder.json` (npm `@kaspacom/kcc20-tx-builder` 0.2.5) |
| Commit | `80e1f7b1f4fb0ad1de8779a1c203ba7edf111679` ("chore: release 0.2.5", 2026-09-25) |
| Licence | Apache-2.0 (upstream `LICENSE`, copied here as `LICENSE`; package.json `license: Apache-2.0`, author KaspaCom) |
| Changes | none to the content. Line endings normalised from CRLF to LF (the repository stores LF): `sha256` of the upstream CRLF file is `2daa74b8097ebca3debed35b8d22fac4c479531725dba9645b1ff56777bff135` |
| Template hash | `911f0638ccb7368bf36d117f1725073ae7ee487ce8b58ca3e8375051c2d40f6c`, see `registry/tokens.json` template `kcc20-kaspacom-0-2-5` (blake3 over prefix 1 B and suffix 25,439 B, recomputed by `kob-protocol` on load and pinned in `artifacts.rs`) |

KOB uses it as a **third-party KCC-20 template** accepted through the registry's strict template list:
the state layout (112 bytes, draft field order), the draft `transfer` / `transfer_delegator` dispatch
tags (`79c71c23` / `fd3ef14a`) and owner scheme `0x04` (covenant id) are what KOB's unmodified KCC-20
order contracts need. Differences from the reference program: up to 8 token inputs and outputs per
transfer, extra entries `burn`, `mint_by_owner`, `mint_public`, `set_public_mint_active` (mint authority
acts on minter UTXOs; there is no freeze or seize entry; but finding K-1, engine PoC in `review_b2_kaspacom.rs` r_kc_05..07:
`set_public_mint_active` plus a `transfer_delegator` input leaves one covenant output unchecked, so the creator of a mint_policy 2
token can mint past max_supply or plant a non-program covenant UTXO), and a 25.5 KB program that every token
input reveals in its signature script. KOB does not own or audit it beyond the engine tests in
`crates/kob-tests/tests/kob_kaspacom_tests.rs`, `kob_kaspacom_v2_tests.rs`, `review_b2_kaspacom.rs` and the pre-audit notes in `registry/tokens.json` (its template is `pending-review`).
