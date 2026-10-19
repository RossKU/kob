# KCC-23 conformance vectors: provenance

| Item | Value |
|---|---|
| File | `conformance.json` (unmodified, byte-for-byte as in the upstream blob) |
| Upstream repository | https://github.com/kaspanet/kccs |
| Upstream path | `kcc-0023/vectors/conformance.json` |
| Source change | `main` commit `1516a6c4cef59940fc3af3096dbcdd9c08690254` ("Add KCC-23: MJ (Metadata JSON) (#26)", 2026-10-14): KCC-23 merged with status **Draft** |
| Git blob sha1 | `143654cbdcab4cffa305cbe025213cceb477d3d8` |
| sha256 | `53bbaad58b0e508ed80c4b6d88524df3d51a270d7d32e1c4b3c82426ba6739e3` (4,711 bytes) |
| `format_version` | 1 |
| Fetched | 2026-10-19, `git show 1516a6c:kcc-0023/vectors/conformance.json` from a clone of kaspanet/kccs |
| Hashing reference of the vectors | kaspanet/rusty-kaspa `cfafeb4c093fa37a303f1b9f19c58f986b870ce3`; KOB runs them against its own pin (v2.1.0, `01b532e`) and every value matches |
| License | The kccs repository is CC0 1.0 Universal (`LICENSE.md`); the vectors are redistributed unchanged |

KCC-23 is a Draft: when upstream changes the vectors, replace the file, update this table and the sha256 in
`crates/kob-tests/tests/kcc23_conformance_tests.rs` (which asserts it, so any edit or drift fails the test suite), and check
`crates/kob-protocol/src/kcc23.rs`, the one module that implements the format.
