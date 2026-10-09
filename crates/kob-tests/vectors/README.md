# Official KCC conformance vectors

Upstream vectors from https://github.com/kaspanet/kccs (CC0 1.0), vendored unmodified and pinned by sha256 in the
tests that execute them. KOB handles any token through adapters, but its own KCC-20 programs, its issuance and every
encoding it labels KCC must pass these.

| Directory | Spec | Upstream source | Executed by |
|---|---|---|---|
| `kcc1/` | KCC-1 Covenant Concepts, Byte Layouts, and ABI (Last Call) | `main` `411b41b`, file from #27 | `tests/kcc1_conformance_tests.rs` |
| `kcc2/` | KCC-2 Authority Schemes (Last Call) | `main` `411b41b`, file from #30 | `tests/kcc2_conformance_tests.rs`, `tests/kcc20_conformance_tests.rs` |
| `kcc20/` | KCC-20 Fungible Token (Last Call) | `main` `3fbec52`, file from #31 | `tests/kcc20_conformance_tests.rs` |

Each directory's `PROVENANCE.md` records the commit, blob id, sha256 and fetch date. Re-vendor when upstream changes
the file (the sha256 assertion fails on any local edit).
