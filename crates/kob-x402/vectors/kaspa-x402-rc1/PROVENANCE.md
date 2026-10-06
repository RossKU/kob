# Provenance

These files are vendored, unmodified copies of the conformance material of the Kaspa x402 binding.

- Source: https://github.com/elldeeone/kaspa-x402
- Tag: v1.0.0-rc.1
- Commit: 040b1ec8335abadbb3c69cf1ea720ae45816b0f7
- License: MIT (text below and in `LICENSE`)
- Vendored: 2026-09-29
- Layout: the original relative paths under the repository's `vectors/` (and `schemas/`) directories are kept.
- Modification: none. Every file is a byte-identical copy; `.gitattributes` (`* -text`) keeps line endings untouched so the
  hashes below stay stable on every platform. `tests/interop_vectors.rs` re-computes each hash and compares it with this list.

Only the exact / standard-native / envelope / payment-identifier material is vendored. The batch-settlement, voucher,
channel-id and tx-v1 vectors are not needed by KOB. The additive (KIP-10) profile is not implemented by KOB; its vector
objects are used only where they exercise profile-independent code (canonical JSON, request-authorization digest, expiry,
finality, HTTP header codec, additive transaction id preimage).

## Known discrepancy: the standard-native vector's fee is below the node's relay floor

`exact/consensus-profiles.json` (standard-native) pays a fee of 200000 sompi. The relay floor of a rusty-kaspa v2.1.0
node for that transaction (1 P2PK input, 2 outputs) is 100 sompi per gram of `max(compute mass, normalized transient
mass)` = 100 × 2036 = 203600 sompi (`mining/src/mempool/check_transaction_standard.rs`; storage mass, 63557 grams here,
is not part of the floor). The transaction is consensus-valid, but a v2.1.0 node refuses to relay it; the live TN10 probe
of `crates/kob-executor/tests/x402_tn10.rs` submits the same shape at 200000 (refused), at the floor minus one sompi
(refused) and at the floor (accepted). The vector stays vendored unmodified; `tests/interop_vectors.rs` checks its
masses and ids, asserts the floor, and runs the mutation cases on a floor-compliant rebuild of the same geometry.

## Files (sha256)

```text
d3b9428b2784b3655c47b51a77c872804f28a891a6b8e77f4b28cf519d413502  exact/consensus-profiles.json
cb9b43e142cdebaf53a96e483dc893caa9c3634aad04b0b253b2f182b0b014ab  exact/interop-v1.json
9749a49ac11b0b84f817c757addea996e86297c4a5dae2778bc07ea7c303e970  negative/accepted-not-offered.json
c5593a0255e6e69a7320b9adc27d26b0159b7c8f10889a8ceceb552edeb87831  negative/amount-overflow.json
609803f1b7f5ea412392cbecbfd050998ff3fc605bc1648fa873f5800e0b4550  negative/exact-additive-missing-challenge.json
1fd68a507b6e462058b031c96bd58c43b11ead843fa4820f63e60e3783f2ff2d  negative/exact-additive-partial-head.json
cdd070624db63c47efb4f87f71cb8278aeb3685d79ae2d735aa2703eea5f877c  negative/exact-transaction-missing-transaction.json
9d3c3882fbf68dd4559e38945bc02b0ee23aa2202eba2de9d47cace6cb1e9360  negative/exact-transfer-unsupported.json
c5c6580a0fa0e6096f58f147c73918e5d49e9d1f473c75361cea4a6189685841  negative/float-amount.json
f7b735d13ba72655ffdc09a761bbf76cfd44c4db2e7bd1363658d654c5dded6b  negative/invalid-asset.json
e501c93f1831f2502e63249459630d2c248c1168d3f3e7997f27548c29af9262  negative/missing-payment-identifier.json
bd64014e4f1e7608f93235b863ab01c91b28675e2491a0032fa3cc53b54b2485  negative/non-colon-network.json
14ddf7fea73088397e6e66e89d316bcd55dd6d55da9ebf16a4104d9f064b6991  negative/outpoint-index-overflow.json
bccec6d9d93e242edd19713cfff5cba3c51b1192c549491eac19e3f46f4a4de9  negative/payment-identifier-conflict.json
bf0e6e63b313b40fe324cbe22b8c4e02c99d8e7dfac5d39af404db96591b71f8  negative/scheme-payload-mismatch.json
17d939825017f262e770b88799b3cb4c806cfbfd7de49a1ac29e7535b1eaebd4  negative/txid-wrong-length.json
da45d0b1c25107167c29af8cb2f800ec9edf2230de5de9c788aeab47d6ef260f  negative/wrong-binding.json
6d5537f9056e39ea6c3442ac8be185c0e1edc5a5e076d6c763e0812e570e78df  negative/wrong-scheme.json
7f314b6f741c960d262172a5cfb6a9d9ca72b4f3bc57e7559bac1287a4392d60  negative/wrong-x402-version.json
feffd1e3c85e73cf2abaa003b0dd896a1a2d377bd0f0cbb85341a3d9eb7aff10  schemas/payment-identifier.schema.json
acb2127ea0ef0cad75d2cdd346b3ea36cf6367a7e25c22246977ae7b2a152754  settlement-response/corrective-402.json
50fdb3d56f3862e9b6ca1a90b0379fad2d58804bbf6956f2835ccc6701a4fe87  settlement-response/failure.json
fb7e84a5690575d74fdcaaff9da8ca0f15f8d6cfb18e337d4c486eb6f1e48c10  x402-http/exact-transaction.json
5cb4b09626704d06efea853dcc370f9817c80d3ae1cf2e52b0a43430aa16eed5  LICENSE
```

## License text

```text
MIT License

Copyright (c) 2026 Kaspa x402 contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

```
