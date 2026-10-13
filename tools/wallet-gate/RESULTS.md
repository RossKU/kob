# Wallet gate results (TN10)

Wallet signing results on testnet-10 (release gate 1, `docs/ops/release.md`). A manual run of the page (`README.md`, "Run")
adds a section like the one below from its downloaded results JSON and `out/page-results.jsonl`. "Valid signature" = the node
accepted the assembled tx (the page cannot verify a v1 Schnorr signature by itself).

## Automated run (2026-09-29, real released extensions driven by Playwright, project-run testnet-10 node)

Fresh random test mnemonics imported into each wallet, wallet switched to testnet-10, `scripts/setup.mjs` run for each wallet address, real popups approved by the
Playwright drivers (`test/wallets/*.mjs`, Playwright Chromium 153.0.8010.12, extension loaded unpacked). Raw data (gitignored): `out/wallet-<name>.json`, screenshots `out/screens/<name>/`.
"Valid signature" = the node accepted the tx built from the wallet's signature and OUR assembled sigscript (KasWare's negative control: same shapes signed with the wrong key were rejected with "script ran, but verification failed", so acceptance is not vacuous).

**Verdict: all three wallets can sign a covenant (P2SH) input in a tx v1 with computeBudget and covenant outputs; TN10 accepted every tx.**
Kastle needs the `scripts` option (plain `signTx` leaves P2SH inputs silently unsigned, issue #353).

| Wallet (version) | T1 P2PK | T2 BidOrder.cancel | T3 KCC-20 transfer | Notes |
|---|---|---|---|---|
| KasWare 0.10.0 (CWS CRX sha256 c2a9cf25...dcf0) | PASS x2 | PASS x2 | PASS x2 | popup "Sign Transaction" every time; returns only `push(sig65)` (66 B) for P2SH inputs, dApp assembles the rest; version, computeBudget and outputs preserved; `getNetwork()` reports `kaspa_testnet_10` (adapter normalises) |
| Kaspire 0.5.1 / release v0.11.37 (zip sha256 8c44f8f9...5527, provider 1.2.0) | PASS x2 | PASS x2 (`ordered-args`) | PASS x2 (`ordered-args`) | wallet-emitted sigscript is byte-identical to ours (T2 895 B, T3 3277 B): independently validates the ABI encoding; broadcasting the wallet-returned tx also passed; `wrap-signature` gives `<sig><redeem>` without tag, `none` gives a bare signature (both signed, not broadcast) |
| Kastle 2.60.1 (CWS CRX sha256 8643bc4f...b5cb) | PASS | plain FAIL (no signature); `scriptHex:""` PASS; `scriptHex`=redeem PASS | plain FAIL (no signature); `scriptHex:""` PASS; redeem PASS | plain call returns the tx with an empty signatureScript and no error (#353); public key is 33 bytes (adapter takes the last 32); redeem variant returns `<sig><redeem>` (890 B / 3159 B), not usable directly, we take the signature |

### Transaction ids (all accepted on TN10)

| Wallet | T1 | T2 | T3 |
|---|---|---|---|
| KasWare | 2107500f6c1f7e9458ffe3e777fe44eaa6ba396d6a730461ed2ca1a7e7c62498, 0a0c8a1cc3735fb8df4005b962ab0514e1912802203460c85d9794338ada6010 | 000090effaa0e835eabb731295d9eee206115cc7b926c6b8ec2bf74ccb24e339, 2a93b18091eb3d7be1de638c40879b7ab60cb3491dbde0fd1fee53bf319abbd3 | 91af44447610e419e7f304909d0775ca91e27a016474369b3caf16c6c92a9a81, d4a59725c4dbec13afcc8d85c0c8f296c2511a95e5a1f7380dcfe7706f181d02 |
| Kaspire | 7f34da2c68aa8a2d38c88d60801dac6a0dfe5a92eac69d0a82d379ec5ee98bdd, e489ba60cba1f2833718836a54d81664ddfc42ee7c50a2753768a91567caebb0 | a916a6b05c26d88aeb10d9fe454c93cf527cc5130206ee2286a792aaec4056be, c3525f50fc8a2ef87631aec78b56bf7b9f31e7efa6dffb9232ac6d35c6e1be28 (wallet-tx: ad2c20793cfdbe03aace21b9816a6b63b833a86433ed732a1a86414c035bd51e) | 35d31aa76f04e74288270105b29240816f88cec37829c6ff5efe635598618745, 3f666ce0ee32742c594afc80024d203a8f9e2c39babc6883698ec34d4aba16fb (wallet-tx: 60140bcd4b196d96c6044ed1fb29094dfdc396c767053023a234c18e9709d5a3) |
| Kastle | 52d0ac7d4616db5bcb9505c03e768acdbb89d1a8814c093853dd65d67c713eb6 | empty-script f1132b6960dc682e5dcfe65d21c8db2b8120e76ce10649b6fc992c2c7c752c99; redeem-script 70bc72ee0f26ca8d27cb745bb755b17ba0f2457fe814c1c3f44cd8929aeb8fa6 | empty-script 95514a7964a6032fd104ba3b34b8b6c80ccf648f2af38c3cd3799f53f31f0942; redeem-script b295b248a1c2956495f45a093a6f795a2e7e76080c7c95fcb9874a167194d689 |

### What the popups displayed (blind signing)

* **KasWare:** "Sign Transaction": input amount, P2SH input address, outputs and fee. T3 shows the covenant input and a "Spend" output to the new owner, **no token amount, no owner change**.
* **Kaspire:** "SECURE APPROVAL / Approve reviewed PSKT?": origin, txid, fee, wallet balance change, per-output "SIGNATURE-BOUND" flag, and the warning "Input 0 is a covenant or non-standard script; Kaspire cannot verify its dApp business rules". Covenant id, redeem script, sighash and mode appear only in the Raw JSON section. No token semantics.
* **Kastle:** confirm popup; the P2SH input value is shown as "Change to your balance" (+2.996 KAS for T2), the T3 covenant output as "Sending amount" (1.993 KAS). Misleading for covenant txs.

### Not covered by this run (still open)

Mainnet; KasWare APK, Kaspire Android/WalletConnect and iOS; multiple/rotated accounts; hardware wallets; multi-input sweep PSKTs and 14 KB txs; how popups behave when the wallet starts on mainnet (Kaspire adapter now re-requests accounts after switching); real users' normal Chrome profiles (drivers used a clean Chromium profile with the extension loaded unpacked, headed for KasWare/Kastle, headless=new for Kaspire).
