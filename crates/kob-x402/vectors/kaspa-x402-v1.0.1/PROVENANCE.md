# Provenance

These files are vendored, unmodified copies of the conformance material of the Kaspa x402 binding.

- Source: https://github.com/elldeeone/kaspa-x402
- Tag: v1.0.1
- Commit: 56221913531bcb8367a900d28ed24c62d50591a1
- License: MIT (text below and in `LICENSE`, the repository's `protocol/LICENSE`)
- Vendored: 2026-10-19
- Layout: the original relative paths under the repository's `protocol/vectors/` (and `protocol/schemas/`) directories are
  kept.
- Modification: none. Every file is a byte-identical copy of the git blob at the tag (`git show v1.0.1:<path>`, LF line
  endings); `.gitattributes` (`* -text`) keeps line endings untouched so the hashes below stay stable on every platform.
  `tests/interop_vectors.rs` re-computes each hash and compares it with this list.

Only the exact / standard-native / envelope / payment-identifier / signature-scope material is vendored. The
batch-settlement, voucher, channel-id, hash-chain and tx-v1 vectors are not needed by KOB. The additive (KIP-10) profile
is not implemented by KOB; its vector objects are used only where they exercise profile-independent code (canonical JSON,
request-authorization digest, expiry, finality, HTTP header codec, additive transaction id preimage). Of
`sighash/consensus.json` the `standard-native` transactions (every consensus-supported hash type) are reproduced; the
batch lifecycle, `single-without-output` and mixed top-up cases are batch-settlement material.

## v1.0.1: the standard-native vector meets the node's relay floor

v1.0.0's `exact/consensus-profiles.json` (standard-native) paid 200000 sompi, below the relay floor of a rusty-kaspa
v2.1.0 node for that transaction (1 P2PK input, 2 outputs: 100 sompi per gram of `max(compute mass, normalized transient
mass)` = 100 x 2036 = 203600 sompi; storage mass is not part of the floor), so a node refused to relay it (reported
upstream; the live TN10 probe of `crates/kob-executor/tests/x402_tn10.rs` showed 200000 refused and the floor accepted).
v1.0.1 pays 300000 sompi and regenerates the dependent ids and signatures (`exact/interop-v1.json`,
`sighash/consensus.json`, `x402-http/exact-transaction.json`; hash-chain and tx-v1 too, which are not vendored).
`tests/interop_vectors.rs` asserts that the vector's fee is at least the floor and that it passes the whole engine path.

Changed against v1.0.0: `exact/consensus-profiles.json`, `exact/interop-v1.json`, `sighash/consensus.json`, `x402-http/exact-transaction.json`.

## Files (sha256)

```text
45d3a9d47195e2d4ded503a6e3fabf5dc9cdd08eee40e4279706ae7bbab19e71  exact/consensus-profiles.json
c27264898915a38b1f03356e308e759584bc23d940348e004dc5c559ccdbcc01  exact/interop-v1.json
bab69d27bb60e33626726fd32461aa20f1b9656a5e34699da69e3c93e21d7efa  negative/accepted-not-offered.json
a712a018512c2c4ee1af8d9abd9d3ba1592b034beb56666a930c24a06d4b68c3  negative/amount-overflow.json
843260f790e97348a18b32c59fa80e2ecaa6004aec26cbc659f94ad16a0bac4a  negative/exact-additive-missing-challenge.json
eb10d59ecf057d46603b70ad694a39fbc12a770ade2328206bcd23c7db86805d  negative/exact-additive-partial-head.json
6105029c39f8d0b6f77705c216fa483c94fde9d9cd5a79bc02350944da59ddbc  negative/exact-payment-flow-missing.json
4c1bfa46fa59765c1575c0a9c54894e7a2bf44be9dad819d71cc9b05083414e5  negative/exact-transaction-missing-transaction.json
ec562e7ff3cc3656f5e7e6a6bb060f965b269a008e4f3555cb81252fa8131572  negative/exact-transfer-unsupported.json
c5818ea41700211b551c59c711df24fad328fd7427c44ccc3266180be2e55ea7  negative/float-amount.json
ce5229370a475d94dbb8aa7b6fae0b3589846b693afe75965b45b70da62a2641  negative/invalid-asset.json
2a51801e6d0a01abaf2c6804ae8b7590b9d2c4627cb44a0b63028037294b31b2  negative/missing-payment-identifier.json
10a4b6739d7a729a603d076af058dfde063c8c94b99f846a597c1215024a8420  negative/non-colon-network.json
481d3249239e9a7ea6bdc40100b45b2b93e2e69dc17532325cb44c922ed98923  negative/outpoint-index-overflow.json
f09eb00cc1ba1d4bd2e5471038f722f844a7133ad70fdef2b8a41b537416a968  negative/payment-identifier-conflict.json
b9dde9bbf164d3f4bba753372597ce38e6b86bc25d6720ee115ec020e418c9a8  negative/scheme-payload-mismatch.json
e8363239ef7f1247c6d1619d7c448370232389c6bcd33bafe0dcaa6433ecf190  negative/txid-wrong-length.json
ade83933a46a634fc059928f6afd38702e36db282bfede3245ad9ef497edb41d  negative/wrong-binding.json
03826a4aff35d023831c146908d80444fb6576611eb4c923f7f33ff8e5e00836  negative/wrong-scheme.json
6aafb6a19c8691d44b88bddcc7122bf4a210e5a030eca50be068e0d11edc2783  negative/wrong-x402-version.json
2a918aa3da7563f81f1de7a264729dc00ca0bb77559933c201efcc060b947cdb  schemas/payment-identifier.schema.json
e33eeee42ac72ec55683fb7ce1ec56f52c6f57c2ce5054a8f1cc675e26c21fb2  settlement-response/corrective-402.json
5549227a11dffd22e15dbd85bce69db9f84a9c97ccf1d84dd8e1f56e9ea21b00  settlement-response/failure.json
12d2c9c736ede4f38a96cbd81f6a48765d3634ddcc946790d83f45339817f001  sighash/consensus.json
217028274bd0d124e6df5168fa1a1e49cc98ba2819b4ebfba1ac0a7832716a24  x402-http/exact-transaction.json
b04a40b32aab46b0229b4de68f936b58f9948e5501e1d003a8afa2345487e59f  LICENSE
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
