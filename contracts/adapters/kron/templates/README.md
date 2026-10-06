# KRON token program templates

Raw KRON token programs (redeem script bytes: 46-byte state prefix followed by the fixed
program suffix). They are public on-chain programs of the KRON launchpad, reconstructed from
the template served by the KRON API (`/api/native/cp-template`) and checked against redeem
scripts observed on mainnet. KOB does not own or audit them; they are pinned here so tests and
allowlist checks compare against exact bytes.

| File | Size | Suffix | Template hash (`silverscript_lang::template::template_hash`) |
|---|---|---|---|
| `kron_token_2433.bin` | 2,433 B | 2,387 B | `2ed46a7edf5b168e67dba56998c58255235bebac436940a85115ca31d5c559f2` |
| `kron_token_2732.bin` | 2,732 B | 2,686 B | `8097c96fe586a785b3ffb62ddd2a9b3012593421d605d136d26153806e28053e` |

The 2,732-byte program is the newer of the two. Both are allowlisted in `registry/tokens.json` (`kron-2433`,
`kron-2732`) and the KRON adapter tests run against both. File sha256 digests are listed in `contracts/SHA256SUMS`.
