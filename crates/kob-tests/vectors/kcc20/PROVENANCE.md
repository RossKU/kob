# KCC-20 conformance vectors: provenance

| Item | Value |
|---|---|
| File | `conformance.json` (unmodified, byte-for-byte as in the upstream blob) |
| Upstream repository | https://github.com/kaspanet/kccs |
| Upstream path | `kcc-0020/vectors/conformance.json` |
| Source change | `main` commit `3fbec524abfbc20e87652eb938db218f8c17db17` ("KCC0 Compliance for KCC-20 + Last Call advancement (#31)", 2026-10-07): pull request #31 merged, KCC-20 Last Call on `main` |
| Git blob sha1 | `5432a8a060b70fec6e19a9b28f56dd93992ab60f` |
| sha256 | `9b424c96ea0e0093cacfd98e3013fd5cbf93dd149cb27696acf288510133010b` (19,275 bytes) |
| `format_version` | 1 |
| Fetched | 2026-10-09, `git show origin/main:kcc-0020/vectors/conformance.json` from a clone of kaspanet/kccs (`origin/main` = `3fbec52`) |
| Same bytes elsewhere | argent-lang/kcc20-reference `c8a0871` `fixtures/kcc20/conformance.json` (the reference program's own snapshot) |
| License | The kccs repository is CC0 1.0 Universal (`LICENSE.md`); the vectors are redistributed unchanged |

Change from the previous copy (PR #31 head `cfb74cf`, blob `e74b90d2...`, sha256 `467c5e7d...`): the borrowed-receive section
gains seven amount-threshold cases (`threshold-zero`, `threshold-negative`, `threshold-negative-unchanged`,
`threshold-negative-decrease`, `threshold-negative-zero`, `threshold-negative-zero-unchanged`,
`threshold-guard-normalized`) and its note says that cases with negative threshold payloads apply only to contracts that
permit them (KCC-20 section 5: a negative threshold behaves as zero, a contract MAY require a non-negative one). Every
other byte is unchanged.

`crates/kob-tests/tests/kcc20_conformance_tests.rs` asserts the sha256 above, so any edit or drift fails the test suite.
The vectors contain no signature verification results ("signature checks are stipulated to succeed", all keys and signatures are
placeholder byte patterns), so the test executes the same structures with real keys; see the header of that test file.
