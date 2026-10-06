# KCC-1 conformance vectors: provenance

| Item | Value |
|---|---|
| File | `conformance.json` (unmodified, byte-for-byte as in the upstream blob) |
| Upstream repository | https://github.com/kaspanet/kccs |
| Upstream path | `kcc-0001/vectors/conformance.json` |
| Pinned commit | `411b41bc14b3fda8f3a0548242c555f3597cad1a` (`main`, "docs: kcc0 is already final (#34)", 2026-10-01) |
| Last change to the file | `da834af024b235171cd154fdfde16c0cadf8854f` ("kcc-1: kcc0 compliance (#27)", Romain Billot, 2026-09-28) |
| Spec status | KCC-1 Last Call (`815ecaf`, "kcc-01: last call status (#32)") |
| Git blob sha1 | `c55d6a8dbce3c0191af97f1d43ae44a34c5a9df1` |
| sha256 | `5a8ae724de9031d3390ded7873df74002ec29a771c5fbc56e1e43d2a9ae26b59` (32,978 bytes) |
| `format_version` | 1 |
| Fetched | 2026-10-02, `git show 411b41b:kcc-0001/vectors/conformance.json` from a clone of kaspanet/kccs |
| License | The kccs repository is CC0 1.0 Universal (`LICENSE.md`); the vectors are redistributed unchanged |

`crates/kob-tests/tests/kcc1_conformance_tests.rs` asserts the sha256 above, so any edit or drift fails the test suite.
Re-vendor (and update the sha256 there) when the file changes upstream; KCC-1 is in Last Call, not Final.
