# Retiring an order template

Every byte of an order covenant is part of its template hash, so any contract change gives the orders placed afterwards
a new script, while every order placed before keeps the OLD script until it is spent. On chain nothing changes for those
orders: the maker's `cancel(sig)` and the permissionless refund / kill of the old covenant still work. What breaks is the
tooling: the builders, the indexer and the wallet pin today's templates, so without explicit support the live orders of a
retired template (their KAS carriers and their custody tokens) could only be ended by special tooling, or by waiting for
an expiry refund that no keeper performs.

## Policy

No order template is retired (no pinned template hash changes) without, in the same change:

1. **The retired artifact kept.** The committed `silverc` artifact that was pinned is copied byte for byte to
   `contracts/retired/<Kind>-<first 8 hex of its hash>.json` (listed in `contracts/SHA256SUMS`) and registered in
   `kob_protocol::retired` (`SOURCES`: the kind it predates, the artifact, its pinned hash, a note with the retirement
   date). The registry checks on first use that the artifact's recorded hash is its bytecode's, equals the pinned retired
   hash, and is not a pinned template of today.
2. **A decoder and the maker's cancel.** `retired::decode` decodes the retired state span with the state type of its
   layout: today's state type of its kind while the layout is today's, otherwise a legacy state type of
   `kob_protocol::retired::lot` for that layout (since protocol v3 every retired template but one has a lot layout: below;
   the protocol v3 cross limit has its own `kob_protocol::retired::nolot` layout, read with `retired::decode_any`, which
   reads every retired template). Fields the old template did not have take the value that means "absent";
   `retired::encode` (`encode_any`) is its exact inverse, and decoding requires the round trip. `build::build_cancel_retired` builds the maker's cancel through the retired script
   (`SigPlan::Retired`): custody, strays of either token and the carrier back to the maker, exactly as
   `build_cancel_order` does for today's templates, under its own compute-budget role
   (`<Kind>.cancel.retired.<hash8>@<program>`, regenerated into `data/compute_budgets.json`). Retired templates are
   SPEND-ONLY: nothing builds an order, a fill, an amend or a refund for them, and `SigPlan::check` accepts only their
   `cancel` entry.
3. **The record-log layout row.** `kob-executor` `indexer::layouts::RETIRED_LAYOUTS` gets the retired layout (code, state
   length, hash, dispatch tags) so record logs written before the change stay readable.
4. **The wallet.** kob-wasm exposes `retiredTemplates`, `decodeRetired`, `retiredScriptPublicKey` and
   `buildCancelRetired`; the web wallet shows an order whose record names a template this build does not pin as an
   "older contract version" (never as closed) and offers its cancel when the template is in `retiredTemplates`.
5. **Engine tests.** The cancel of a live order of the retired template validates in the rusty-kaspa engine for every
   token program it supports (`crates/kob-protocol/tests/retired_cancel.rs` is the pattern).

The reverse direction (a user on a cached OLD web bundle placing orders under a template the new indexer ignores) is
covered by the web bundle's pinned template hashes: a wallet refuses to place what its build does not pin, and the
indexer reports unknown templates as `unknown_reveals`.

Router intent templates (`contracts/argent/router`) are not order templates and have no retirement path: an intent
lives until its deadline, after which anyone (the x402 facilitator first) expires it. On 2026-10-06 the lock pin
(`docs/argent.md`, "Lock pin") changed the 24 token-intent templates (`TokenToKas*`, `TokenSwap*` and their KRON
twins; the `KasToToken*` templates are unchanged). An intent created from a previous template keeps its script: its
payer's `cancel` and anyone's `expire` still validate on chain, but today's builders do not build for it, and the lock
stand-in it was vulnerable to still works on it until it ends.

## Retired templates this build can spend

Every order template a testnet-10 build pinned (the real-wallet runs of 2026-09-29 from the reference build, the TN10 soak
from 5e8a249 on with `--features deploy-tn10`, whose receipt-era build took the R_ID-dependent KCC-20 conditional and
if-done templates from `contracts/deploy/testnet-10`) and is no longer pinned, plus the first cross limit of `b75b3e5`.
Protocol v3 (`8dd4ebf`, 2026-10-05: amounts in base units, no lots) retired every template of protocol v2.6, the 14
pinned by `36d569c`. The protocol v3 cross limit itself (`8dd4ebf`) was replaced by the pair orders (`82672b9`) before
any deployment; it is retired for safety all the same.
The deployments, from the artifacts committed at each commit: real-wallet runs `a1a301a` / `8def524` (2026-09-29) and
soak `5e8a249` (2026-09-29/30): receipt era; soak `5e4c00e` (2026-10-01): v2.6 with the 333-byte cross limit; soak
`84d8efd`, `eaa93db` and the cherry-picked planner fix (2026-10-01): v2.6 with the pair-market auction; soak `ae87d77`
(2026-10-02, deployed by `5eb73e8`): cost pass 1 with the if-done entries retired on 2026-10-02 and the other templates
retired on 2026-10-05; soak `6b78df6` (`406feca`, 2026-10-02), `29f0c64` (2026-10-02), `5a3e9a3` (`608c66d`, 2026-10-02)
and `747797c` (`acd2c14`, 2026-10-03, the final soak B6), the last deployment before protocol v3: the 14 templates
retired on 2026-10-05 (main kept them unchanged up to `36d569c`).

| Kind | Hash | State | Retired | Note |
|---|---|---|---|---|
| `KobIfdAsk` | `189b9c3297cac33c0de6b5defefcbee5deee42306e8aa72b4615d8f2d4515a17` | 594 B (today's layout) | 2026-10-06 (security review) | sell-first entry of protocol v3 whose refund (`settle` n = 0) did not require tokens held: an empty repeating entry could be drained by anyone through a zero-amount stand-in custody after its soft expiry; only the code changed (decoded with today's state type) |
| `KobIfdAskKron` | `85d8783813f861d16d2679d13fd062d81ce572f7fe4efa111943bd5a6a6b4ac8` | 561 B (today's layout) | 2026-10-06 (security review) | KRON sell-first entry of protocol v3, the same refund fix; only the code changed |
| `KobAsk` | `66853f1603149ff6371a06655ac58fa9f775ad80fce0141876c31dc41364c3c2` | 243 B (v2.6 lot layout) | 2026-10-05 (`8dd4ebf`) | limit sell, the last lot template (cost pass 1, a7072a1 2026-10-01); TN10 soak ae87d77 to 747797c |
| `KobAskKron` | `3389eeb2e971cacbbbbda193582d38db5f1bf56acff55478d9afd4a4d956b7d7` | 243 B (v2.6 lot layout) | 2026-10-05 (`8dd4ebf`) | KRON limit sell, the last lot template (a7072a1 2026-10-01); pinned by the TN10 builds ae87d77 to 747797c |
| `KobBid` | `208589746fefe2ea9082cef18b75b0e5a31f9e1b059421dc0caeef96b3d55728` | 285 B (v2.6 lot layout) | 2026-10-05 (`8dd4ebf`) | limit buy, the last lot template (a7072a1 2026-10-01); TN10 soak ae87d77 to 747797c |
| `KobBidKron` | `a116125531ae2aaa1218a836c6891c11174faa15427da34672c7b11d19f0151e` | 252 B (v2.6 lot layout) | 2026-10-05 (`8dd4ebf`) | KRON limit buy, the last lot template (a7072a1 2026-10-01); pinned by the TN10 builds ae87d77 to 747797c |
| `KobCondAsk` | `fe74058698d86e6c22aae188441e853398f770a71c53f8a2b3d7229821e7d3ff` | 330 B (v2.6 lot layout) | 2026-10-05 (`8dd4ebf`) | conditional sell, the last lot template (a7072a1 2026-10-01), also the exit the if-done entries `ca757658` and `35ed6bed` inline; TN10 soak ae87d77 to 747797c |
| `KobCondAskKron` | `219c53b28b33579f699fdb93cff1c7c3f2bba2ee4033a1a22cfde6980807343e` | 330 B (v2.6 lot layout) | 2026-10-05 (`8dd4ebf`) | KRON conditional sell, the last lot template (a7072a1 2026-10-01), also the exit of `354fb5de` and `d16e03ff`; pinned by the TN10 builds ae87d77 to 747797c |
| `KobCondBid` | `3a5f1e2227c115360a2ef4143f597d31c6e1e5e00f44244ab22851f5458680d3` | 381 B (v2.6 lot layout) | 2026-10-05 (`8dd4ebf`) | conditional buy, the last lot template (a7072a1 2026-10-01), also the exit the if-done entries `138d4fd8` and `789ddaa1` inline; TN10 soak ae87d77 to 747797c |
| `KobCondBidKron` | `0cc7eeb9b02f90f0fa031acb48826dba4b35ce614ea6e659ef4772ebe38f9cb4` | 348 B (v2.6 lot layout) | 2026-10-05 (`8dd4ebf`) | KRON conditional buy, the last lot template (a7072a1 2026-10-01), also the exit of `fd13762a` and `6d19a13d`; pinned by the TN10 builds ae87d77 to 747797c |
| `KobIfdBid` | `ca757658e4872b64efb9a25c39aeefb68a2622dcc93b04426261377e2bb4d199` | 594 B (v2.6 lot layout) | 2026-10-05 (`8dd4ebf`) | buy-first entry, the last lot template (69c9014 2026-10-02); TN10 soak 6b78df6 to 747797c |
| `KobIfdBidKron` | `354fb5de47e5f9141da32cb3d8f282323200d915b9bacbec126a370479a8c232` | 561 B (v2.6 lot layout) | 2026-10-05 (`8dd4ebf`) | KRON buy-first entry, the last lot template (69c9014 2026-10-02); pinned by the TN10 builds 6b78df6 to 747797c |
| `KobIfdAsk` | `138d4fd83941a072ceaaa3e1ce95f565f2a8ec21e529d43287245839b17a79e8` | 603 B (v2.6 lot layout) | 2026-10-05 (`8dd4ebf`) | sell-first entry, the last lot template (69c9014 2026-10-02); TN10 soak 6b78df6 to 747797c |
| `KobIfdAskKron` | `fd13762a9157ff89b996f38bd9641937ab983a7c7375537b356747a972043540` | 570 B (v2.6 lot layout) | 2026-10-05 (`8dd4ebf`) | KRON sell-first entry, the last lot template (69c9014 2026-10-02); pinned by the TN10 builds 6b78df6 to 747797c |
| `KobCross` | `ea23f1fec9719d56a31f4b54b15cc3e6a286af0384b7e7769f571dfd39a74b89` | 360 B (v3 no-lot layout, `retired::nolot`) | 2026-10-05 (pair orders, `82672b9`) | cross limit of protocol v3 (`8dd4ebf`, 2026-10-05): amounts in base units, one template for token A of both families (`aFamily`); never deployed (no live order known), retired for safety; its cancel validated for token A and B of every family |
| `KobCross` | `692cfdd07dae71749db2d0ad8249eb70c7476e9ef67a4f8f4c09c409e991b752` | 351 B (v2.6 lot layout) | 2026-10-05 (`8dd4ebf`) | cross limit (token A KCC-20), the last lot template (a7072a1 2026-10-01); TN10 soak ae87d77 to 747797c |
| `KobCrossKron` | `ef254324cc270929a9b5b70116af1739983faa3dd4bb7278dc5b01044d631354` | 351 B (v2.6 lot layout) | 2026-10-05 (`8dd4ebf`) | KRON cross limit (token A KRON), the last lot template (a7072a1 2026-10-01); pinned by the TN10 builds ae87d77 to 747797c; protocol v3 has no KRON twin of the cross limit (`KobCross` takes `aFamily`) |
| `KobCross` | `70b1dcd3e5f2c3821f031d5346680d0772fb37851760aea8fa841e80424a92c7` | 333 B | 2026-10-01 (`58cc732`) | cross limit (v2.6, from c8f3b01 2026-09-30) before the pair-market auction; TN10 soak 5e4c00e |
| `KobCrossKron` | `7371a59d8d5502089e7dbdff1d2f78b52028d58a70d2c037759895f384578f87` | 333 B | 2026-10-01 (`58cc732`) | KRON cross limit (v2.6, from c8f3b01 2026-09-30) before the pair-market auction; pinned by the TN10 build 5e4c00e |
| `KobIfdBid` | `35ed6bed435f75faaac2f2c7f843312af9d5a4e5a6d404f34ad1ac0690feebc7` | 594 B (v2.6 lot layout) | 2026-10-02 (`69c9014`) | buy-first entry before update required lots and the stop below the limit; TN10 soak ae87d77 |
| `KobIfdAsk` | `789ddaa134f29a1f5fe23387963e05c51d29c737d2d44a196823a5623df3dad3` | 603 B (v2.6 lot layout) | 2026-10-02 (`69c9014`) | sell-first entry before update required lots and the stop above the limit; TN10 soak ae87d77 |
| `KobIfdBidKron` | `d16e03fff6199106e85790c7fcc962269f0f2019521507f720b063841b2429a6` | 561 B (v2.6 lot layout) | 2026-10-02 (`69c9014`) | KRON buy-first entry before update required lots and the stop below the limit; pinned by the TN10 build ae87d77 |
| `KobIfdAskKron` | `6d19a13d97d88187f48b3fac6a6cd6bf0f8d50f05f64c4b2069237fed90459ac` | 570 B (v2.6 lot layout) | 2026-10-02 (`69c9014`) | KRON sell-first entry before update required lots and the stop above the limit; pinned by the TN10 build ae87d77 |
| `KobAsk` | `ccc62fa3730ada83b7b0e1bedcef4fc9635742a5a1972cf918be6639cf73ed02` | 243 B (v2.6 lot layout) | 2026-10-01 (`a7072a1`) | limit sell (v2.4 M6 to v2.6, from 2a94c3c 2026-09-30) before cost pass 1; TN10 soak 5e4c00e to eaa93db |
| `KobAskKron` | `452ec1f8ea9f4debbe8182809ed945b83c6e55572675ad64caffb9f1972054ab` | 243 B (v2.6 lot layout) | 2026-10-01 (`a7072a1`) | KRON limit sell (v2.4 M6 to v2.6, from 2a94c3c 2026-09-30) before cost pass 1; pinned by the TN10 builds 5e4c00e to eaa93db |
| `KobBid` | `39af759ef52f23a265efb6ea9c7b857c8a777940dae5aa65b8c7ae4e5e360d09` | 285 B (v2.6 lot layout) | 2026-10-01 (`a7072a1`) | limit buy (v2.3 to v2.6, from a85c793 2026-09-29) before cost pass 1; real-wallet TN10 runs 2026-09-29, TN10 soak 5e8a249 to eaa93db (the 6 unlocated KAS-only bids of the 10-01 flood) |
| `KobBidKron` | `87c84a09979ee4b097ea0b7c2ef1bec48d0d765c2b820136278c17f75f9ccf96` | 252 B (v2.6 lot layout) | 2026-10-01 (`a7072a1`) | KRON limit buy (v2.3 to v2.6, from 119f76b 2026-09-29) before cost pass 1; pinned by the TN10 builds 5e8a249 to eaa93db |
| `KobCondAsk` | `d17183a89c58a7eef3f0bbd0f501f0161721a1a15565bf860defe1fdecd7c991` | 330 B (v2.6 lot layout) | 2026-10-01 (`a7072a1`) | conditional sell (v2.6 touch trigger, from d849ef1 2026-09-30) before cost pass 1; TN10 soak 5e4c00e to eaa93db |
| `KobCondAskKron` | `a2f64f94a9e1e4e86b2fd97c2b97ef92d0398597d32b5fe181318df44e573128` | 330 B (v2.6 lot layout) | 2026-10-01 (`a7072a1`) | KRON conditional sell (v2.6 touch trigger, from d849ef1 2026-09-30) before cost pass 1; pinned by the TN10 builds 5e4c00e to eaa93db |
| `KobCondBid` | `f959c978dde45ad2ff23498a742ca489617cea411cb6d933264e47b2d2c26ed6` | 381 B (v2.6 lot layout) | 2026-10-01 (`a7072a1`) | conditional buy (v2.6 touch trigger, from d849ef1 2026-09-30) before cost pass 1; TN10 soak 5e4c00e to eaa93db |
| `KobCondBidKron` | `339a1bb18e291f1a94bd0faafa3ece05d0bdbd9ee84d370830df6fe8f97cff45` | 348 B (v2.6 lot layout) | 2026-10-01 (`a7072a1`) | KRON conditional buy (v2.6 touch trigger, from d849ef1 2026-09-30) before cost pass 1; pinned by the TN10 builds 5e4c00e to eaa93db |
| `KobIfdAsk` | `8afde0646e060be0efd60c263aa0d719106046aa95dcc8a4aa79c625f69b2e93` | 603 B (v2.6 lot layout) | 2026-10-01 (`a7072a1`) | sell-first entry (v2.6 touch trigger, from d849ef1 2026-09-30) before cost pass 1; TN10 soak 5e4c00e to eaa93db |
| `KobIfdAskKron` | `3c73e5c4e0a4dd1278e7cbec7586ff7fe6eaa8fcad19af3a111e43b0f19bcbc8` | 570 B (v2.6 lot layout) | 2026-10-01 (`a7072a1`) | KRON sell-first entry (v2.6 touch trigger, from d849ef1 2026-09-30) before cost pass 1; pinned by the TN10 builds 5e4c00e to eaa93db |
| `KobIfdBid` | `824b42ee47df65e236fef29896225c9a39cdd4153fc8698957f6c826fbcb79ca` | 594 B (v2.6 lot layout) | 2026-10-01 (`a7072a1`) | buy-first entry (v2.6 touch trigger, from d849ef1 2026-09-30) before cost pass 1; TN10 soak 5e4c00e to eaa93db |
| `KobIfdBidKron` | `6af8728dab4e5daf75d57f05c2cc3bf5e1c9d94b6ee639046854ef02d6f9bc62` | 561 B (v2.6 lot layout) | 2026-10-01 (`a7072a1`) | KRON buy-first entry (v2.6 touch trigger, from d849ef1 2026-09-30) before cost pass 1; pinned by the TN10 builds 5e4c00e to eaa93db |
| `KobCross` | `b23ae732e42b52bf04dc9fde31a1c2d6601cede9c3a1151d102b9933ee32ec4b` | 351 B (v2.6 lot layout) | 2026-10-01 (`a7072a1`) | cross limit with the pair-market auction (58cc732, 2026-10-01) before cost pass 1; TN10 soak 84d8efd, eaa93db |
| `KobCrossKron` | `87a01b1dcbb079d4bd9811789cdee6195b9e1ca596853dd8ece09a3359d5eca0` | 351 B (v2.6 lot layout) | 2026-10-01 (`a7072a1`) | KRON cross limit with the pair-market auction (58cc732, 2026-10-01) before cost pass 1; pinned by the TN10 builds 84d8efd, eaa93db |
| `KobCross` | `0b1467488070f986ade9237c8af63b09404054c7f9ae91b3d60ba18e2a152929` | 333 B | 2026-09-30 (`c8f3b01`) | first cross limit (b75b3e5, 2026-09-30) before a cross limit with A = B could be refunded |
| `KobCrossKron` | `d1d8d49890ff9325523f616b810912e386639a355415b10bc62b70625d6196bc` | 333 B | 2026-09-30 (`c8f3b01`) | first KRON cross limit (b75b3e5, 2026-09-30) before a cross limit with A = B could be refunded |
| `KobAsk` | `21cf26fc19afe61423645f5a80bbef0255a5438df15f6cd753d89c440edc7b2d` | 243 B (v2.6 lot layout) | 2026-09-30 (`2a94c3c`, v2.4 M6 fixes: positional IOC return) | limit sell (v2.3 to v2.5, from a85c793 2026-09-29); real-wallet TN10 runs 2026-09-29, TN10 soak 5e8a249 |
| `KobAskKron` | `301774bd22a192e52ac565b08e2c9488896348aa1eaa8a8e94e460d439f5f82f` | 243 B (v2.6 lot layout) | 2026-09-30 (`2a94c3c`, v2.4 M6 fixes) | KRON limit sell (v2.3 to v2.5, from 119f76b 2026-09-29); pinned by the TN10 build 5e8a249 |
| `KobCondAsk` | `92d36ff2ecad7a04679b41de616c56cf8c633d8179a4acedec9fff8eae82ae28` | 330 B (v2.6 lot layout, `minRcptUnits`) | 2026-09-30 (`2a94c3c`) | conditional sell of the receipt era (v2.4/v2.5, 3826d9f 2026-09-29), reference build (placeholder R_ID); real-wallet TN10 runs 2026-09-29 (OCO) |
| `KobCondAskKron` | `695917526f63e76cfbcc45f65e82f0a606699427115cb1adf5519f99a625b3d3` | 330 B (v2.6 lot layout, `minRcptUnits`) | 2026-09-30 (`2a94c3c`) | KRON conditional sell of the receipt era (v2.4/v2.5, 119f76b 2026-09-29), placeholder R_ID; pinned by the TN10 build 5e8a249 |
| `KobCondBid` | `5ed4931a49d7370dc2c77512f3fb7866dc5051014493f306b0b3d80b6ac25196` | 381 B (v2.6 lot layout, `minRcptUnits`) | 2026-09-30 (`2a94c3c`) | conditional buy of the receipt era (v2.4/v2.5, 3826d9f 2026-09-29), reference build (placeholder R_ID); pinned by the real-wallet TN10 build 2026-09-29 |
| `KobCondBidKron` | `5c1106498d6572d683f7d75afca22654aa650dc366733fb7e80d070d60ad7ee9` | 348 B (v2.6 lot layout, `minRcptUnits`) | 2026-09-30 (`2a94c3c`) | KRON conditional buy of the receipt era (v2.4/v2.5, 119f76b 2026-09-29), placeholder R_ID; pinned by the TN10 build 5e8a249 |
| `KobIfdAsk` | `526b6b77322c1e620d51dacc7c31678d8e3f2ec28d11077a4bdf02d272185df5` | 603 B (v2.6 lot layout, `minRcptUnits`) | 2026-09-30 (`2a94c3c`) | sell-first entry of the receipt era (v2.4/v2.5, 3826d9f 2026-09-29), reference build (placeholder R_ID); pinned by the real-wallet TN10 build 2026-09-29 |
| `KobIfdAskKron` | `de56abde8b6bfa99c2eb32589443907fb4bfd15fd9564756f991cb4badd7fe98` | 570 B (v2.6 lot layout, `minRcptUnits`) | 2026-09-30 (`2a94c3c`) | KRON sell-first entry of the receipt era (v2.4/v2.5, ada6d8d 2026-09-29), placeholder R_ID; pinned by the TN10 build 5e8a249 |
| `KobIfdBid` | `c3c9e2067fe67ee932d2fa848c827e2907fa6c4ddb8237e4f42b775c0a62579a` | 594 B (v2.6 lot layout, `minRcptUnits`) | 2026-09-30 (`2a94c3c`) | buy-first entry of the receipt era (v2.4/v2.5, 3826d9f 2026-09-29), reference build (placeholder R_ID); real-wallet TN10 runs 2026-09-29 (IFD) |
| `KobIfdBidKron` | `0784282f33c59af7eee628c90c63430639817f246faaa9aba80a53ed47e08703` | 561 B (v2.6 lot layout, `minRcptUnits`) | 2026-09-30 (`2a94c3c`) | KRON buy-first entry of the receipt era (v2.4/v2.5, 119f76b 2026-09-29), placeholder R_ID; pinned by the TN10 build 5e8a249 |
| `KobCondAsk` | `9fa11943c280056239e61f3690d734adbfd5667e8ec8b1721fe6bcb794f8fe9f` | 330 B (v2.6 lot layout, `minRcptUnits`) | 2026-09-30 (`2a94c3c`) | conditional sell, testnet-10 deployment build of the receipt era (6db1f08 2026-09-29, R_ID of receipt genesis 64ae62c1); TN10 soak 5e8a249 |
| `KobCondBid` | `040f5ac8e23b4360a7956028fa05d3f370110cbe1b13fe87c6213317d17e32ad` | 381 B (v2.6 lot layout, `minRcptUnits`) | 2026-09-30 (`2a94c3c`) | conditional buy, testnet-10 deployment build of the receipt era (6db1f08 2026-09-29, R_ID of receipt genesis 64ae62c1); TN10 soak 5e8a249 |
| `KobIfdAsk` | `dd3054d09dfedab7244735bfd2f5e8ba46c0be8cbce01f9463033c94c8f13dab` | 603 B (v2.6 lot layout, `minRcptUnits`) | 2026-09-30 (`2a94c3c`) | sell-first entry, testnet-10 deployment build of the receipt era (6db1f08 2026-09-29, R_ID of receipt genesis 64ae62c1); TN10 soak 5e8a249 |
| `KobIfdBid` | `ef5a575fbf1003bf3779be7efa169c795dc39a03348123091a0ed319fc42eeda` | 594 B (v2.6 lot layout, `minRcptUnits`) | 2026-09-30 (`2a94c3c`) | buy-first entry, testnet-10 deployment build of the receipt era (6db1f08 2026-09-29, R_ID of receipt genesis 64ae62c1); TN10 soak 5e8a249 |

Every retired template has a LOT layout (`unit`, `lotUnits`, `lotsLeft`, `tipLot`, ...), different from today's (protocol
v3: `scale`, `minFill`, `amountLeft`, `tip`, ...; the if-done and cross limit states also changed length): no retired
state decodes with today's decoder. "v2.6 lot layout" means the layout of the protocol v2.6 templates retired on
2026-10-05 (the same length and the same fields in the same order with the same types): those states decode with the
legacy lot state types of `kob_protocol::retired::lot` (the v2.6 state structs), and `retired::encode` is their exact
inverse. Retired orders are cancel only: the maker's cancel is built from the maker, the token(s), the custody amount
`lotsLeft × lotUnits × unit` and the strays, never from today's state. `minRcptUnits`: the receipt-era conditional and
if-done kinds (protocol v2.3 to v2.5) have the receipt minimum (the least receipt size that arms a stop) where the v2.6
lot layout has `minTouchUnits` (the least fill that touches it), at the same position with the same type; the old value
is carried in that field (`retired::RENAMED`). Only a stop reads that field, never a cancel, which needs the maker, the
token(s) and the lots (custody). The 333-byte cross limits are the 351-byte v2.6 lot state without `bLotEnd` and
`auctionDaa` (offset 324, two 9-byte integer pushes): `bLotEnd = bLot`, `auctionDaa = 0` (no auction), and
`retired::encode` refuses a state with an auction.

**Retired with today's layout.** The protocol v3 sell-first entries `KobIfdAsk` `189b9c32` and `KobIfdAskKron` `85d87838`
(retired 2026-10-06) differ from today's templates only in code: their refund now requires `amountLeft > 0` (an entry with
nothing left ends by `close`, which pays the maker). Their states are today's (`Retired::is_current_layout`), so
`retired::decode_any` reads them as `RetiredState::Current` (today's state type of the kind; kob-wasm `decodeRetired` answers
today's `{"kind", "state"}`), and the cancel is built as for every retired template. Whether orders of them are live on
testnet-10 is not derived from git; every hash is covered regardless.

**Replaced before deployment, retired for safety.** The protocol v3 cross limit `KobCross`
(`ea23f1fec9719d56a31f4b54b15cc3e6a286af0384b7e7769f571dfd39a74b89`, 360 B, one template for both families of token A),
pinned from `8dd4ebf` (2026-10-05) and listed in the deployment records regenerated by `43d86a9`, was replaced by the
pair orders (`KobPair`, `KobCondPair`, `KobIfdPair`, `82672b9`) before any build of it ran on a network, so no order of
it is known. It is retired all the same, so that an order nobody knows of can still be cancelled: its artifact
(`contracts/retired/KobCross-ea23f1fe.json`, byte for byte from `8dd4ebf`), its own state type
(`kob_protocol::retired::nolot::CrossState`: amounts in base units, `aFamily`, `scale`, `minFill`, the B token,
`price` / `priceEnd` / `auctionDaa`, `amountLeft`; read by `retired::decode_any`, while the lot reader `retired::decode`
refuses it) and the maker's cancel (the custody of exactly `amountLeft` of A and the strays of A and B back to the maker;
the family of token A is its `aFamily`, checked against the custody's program). Kind `0x08` of payload version 4 names
`KobPair`; version-3 records of kind `0x08` keep naming the retired lot cross limits above. No other template changed.

A retired template's cancel has its own compute-budget role, `<Kind>.cancel.retired.<hash8>@<program>` (an older template
is often larger: the sell-first entry `8afde064` needs 11 budget units where the other v2.6 cancels needed 10), measured with the
other roles by `crates/kob-protocol/tests/budget_table.rs` (`common::retired_orders`). A new retired template gets its rows
by regenerating the table (`KOB_REGEN=1`).

The booked exit of a retired if-done entry is an order of the conditional template that entry inlines, itself retired:
the conditional templates retired on 2026-10-05 for the entries retired on 2026-10-05 and 2026-10-02, the retired
conditional template of the same generation for the older entries (all listed above: cancel only). The merge back into a
retired entry is never built (spend-only).

Not covered (live orders on them need special tooling until they are added the same way):

* **Templates no testnet-10 build pinned.** The v2.4 M6 generation (`2a94c3c` to `d849ef1`, 2026-09-30: `KobCondAsk`
  `4f9692d4`, `KobCondBid` `6cf8eba2`, `KobIfdAsk` `34c4e331`, `KobIfdBid` `f77e5897`, the KRON twins `adda1e31`,
  `c6552b4e`, `987a2d40`, `ac5df87b`, and its testnet-10 deployment build `5cafde88`, `6f4eb639`, `0acad08d`, `6a4f2fe4`,
  which `2a94c3c` marked stale: it needed a new receipt genesis) fell between the soak's `5e8a249` and `5e4c00e`
  deployments; protocol v2 to v2.2 (`cf8a6b7` to `52c064b`, 2026-09-29 morning) was pinned by no TN10 build of this
  history (the wallet-gate runs of that morning used the v1 `BidOrder`, not a KOB template). They stay rows of
  `RETIRED_LAYOUTS` (record logs) only. The v2 to v2.2 states are shorter than the v2.6 lot layout (for example `KobAsk` 198 / 225
  bytes) and would need a real decoder per layout; the v2.4 M6 generation has the v2.6 lot layout with `minRcptUnits` and
  would only need its artifacts and rows here.
* **The trade receipts** (`KobReceipt` `8edb9304` / `KobReceiptKron` `a6890844` of the receipt era, pinned by `5e8a249`):
  not an order kind (protocol v2.6 deleted the kind, there is no state type to decode into) and without a `cancel`
  entry: a receipt ends through its own `retire` entry, which the soak's wind-down before the `5e4c00e` redeploy ran.

Other limits: the indexer does not keep retired-template orders as unlisted rows with a state (no views, no keeper
refunds); a maker cancels them from the placement record. Which of these orders are still live on testnet-10 is not
derived from git: by the soak notes, every wind-down cancelled its orders except the 6 KAS-only bid placements
(`KobBid` `39af759e`) and one cross-limit cancel the 2026-10-01 flood left unlocated, and the real-wallet runs of
2026-09-29 cancelled what they placed (`web/e2e-real/README.md`); every hash above is covered regardless.
