# Hallmarks v1.0 (vendored data, own implementation)

Source: <https://github.com/GBKS/hallmarks> by Christoph Ono, npm `hallmarks` 1.0.0 (released 2026-05-13, spec dated 2026-05-13).
Pinned release tarball: `sha512-ffNt23iz7AyOt5m92GoQTiZoMMHsncPtgJr1NTWC97LVpb3GHgmyl236jnoO8J+APi4v2DxyqDesL5E30cLTWw==` (sha1 `b4affabe7b33bc8e240dd8df2c904aaa11b15e51`).

| File here | What it is | Licence |
| --- | --- | --- |
| `hallmark.ts` | KOB's own implementation of SPEC.md sections 3.1 to 3.7 (the "standard" style): SHA-256, Mulberry32, the 5x7 pattern, OKLCH colours, the three words. Written from the spec, not copied. | ISC (this repository) |
| `bip39-english.ts` | The 2048-word BIP-39 English wordlist, extracted from the reference `hallmark.js`. Its SHA-256 (words joined by LF, one LF at the end) is `2f5eed53a4727b4bf8880d8f3f199efc90e58503646d9ff8eff3a2ed3b24dbda`, the digest of the official list. | public list, MIT in the reference |
| `test-vectors.json` | The reference conformance vectors, byte for byte (SHA-256 `661e880331cd2d98f3103b870d84deb2d8daaa454978fd14a465e1a507678879`). | CC0 1.0 |
| `LICENSE` | The reference licence (MIT, plus the CC0 dedication of the spec and the vectors). | |

`hallmark.test.ts` reproduces every vector (hash, cells, words, standard colours) and checks both pins. A conformant implementation reproduces
every vector exactly (SPEC.md section 7); a change that breaks one is a different algorithm, which is the swap `web/src/app/fingerprint.ts`
exists for.

KOB feeds it the lowercase 64-hex covenant id. It is a comparison aid, not a commitment: two ids with the same picture can still differ, and the
full id (copy button, tooltip, token page) is what identifies a token. Not used: the 14x20 pixel form, the other two styles, the border.

To update: fetch the new release, compare its vectors with these, and re-pin the digests above and in `hallmark.test.ts`.
