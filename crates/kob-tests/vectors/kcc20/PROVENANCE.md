# KCC-20 conformance vectors: provenance

| Item | Value |
|---|---|
| File | `conformance.json` (unmodified, byte-for-byte as in the upstream blob) |
| Upstream repository | https://github.com/kaspanet/kccs |
| Upstream path | `kcc-0020/vectors/conformance.json` |
| Source change | Pull request #31 (KCC-20 kcc-0 compliance, Last Call), head commit `cfb74cfa4d7e25f3e7ec0f9144ad72c460b9d810` ("Clarify hash-chain borrow security model", author Manyfestation, 2026-09-28) |
| Git blob sha1 | `e74b90d2dba8a4ea8f3dab9253ad68e17a1e04b6` |
| sha256 | `467c5e7d24c0b44c7cf25b122f61922493466afd7529e0bd7fd23ac7847e724e` (15,746 bytes) |
| `format_version` | 1 |
| Fetched | 2026-09-29, `git show pr31:kcc-0020/vectors/conformance.json` from a clone of kaspanet/kccs after `git fetch origin pull/31/head` (FETCH_HEAD = cfb74cf) |
| License | The kccs repository is CC0 1.0 Universal (`LICENSE.md`); the vectors are redistributed unchanged |

The file is a copy of an unmerged pull request, not a Final specification artifact: the PR is open, KCC-20 is Last Call, and the
bytes have changed three times in six weeks. Re-vendor (and update the sha256 in
`crates/kob-tests/tests/kcc20_conformance_tests.rs`) when the PR changes or merges.

`crates/kob-tests/tests/kcc20_conformance_tests.rs` asserts the sha256 above, so any edit or drift fails the test suite.
The vectors contain no signature verification results ("signature checks are stipulated to succeed", all keys and signatures are
placeholder byte patterns), so the test executes the same structures with real keys; see the header of that test file.

Re-checked 2026-10-02 (after KCC-1 #27 and KCC-2 #30 reached Last Call on `main`): PR #31 head is still `cfb74cf`, so
this file is current. The PR is not yet rebased onto the new KCC-1 / KCC-2 texts; see `docs/spec/kcc-conformance.md`
section 5 for what that may change.
