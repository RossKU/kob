# KCC-2 authority-scheme vectors: provenance

| Item | Value |
|---|---|
| File | `authority-schemes.json` (unmodified, byte-for-byte as in the upstream blob) |
| Upstream repository | https://github.com/kaspanet/kccs |
| Upstream path | `kcc-0002/vectors/authority-schemes.json` |
| Pinned commit | `411b41bc14b3fda8f3a0548242c555f3597cad1a` (`main`, "docs: kcc0 is already final (#34)", 2026-10-01) |
| Last change to the file | `90beacd0dd34543a7321144d122189d779c5ad9b` ("kcc-2: kcc-0 compliant and use unkeyed hash function (#30)", Romain Billot, 2026-09-27) |
| Spec status | KCC-2 Last Call (`ad1b899`, "kcc-02: last call status (#33)") |
| Git blob sha1 | `6db2925ae5d54f1ab29dcc0f0c403e078d6bcafb` |
| sha256 | `1ff8e2169203b7e52a065fee20c8551b059fb955dc69a91c332ffd6f7cde5835` (9,311 bytes) |
| Fetched | 2026-10-02, `git show 411b41b:kcc-0002/vectors/authority-schemes.json` from a clone of kaspanet/kccs |
| License | The kccs repository is CC0 1.0 Universal (`LICENSE.md`); the vectors are redistributed unchanged |

`crates/kob-tests/tests/kcc2_conformance_tests.rs` asserts the sha256 above. The vectors' public keys are secp256k1
multiples of the generator (`79be...` = 1G, `c6047f...` = 2G, `0379be...` = -G), so the program-level test in
`crates/kob-tests/tests/kcc20_conformance_tests.rs` signs with the matching secret keys (1, 2, n-1) and executes every
approval check in KOB's KCC-20 programs.
