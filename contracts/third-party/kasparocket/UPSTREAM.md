# KaspaRocket token program (third-party, observed on chain): `KaspaRocketToken11458`, mainnet and testnet-10, reviewed

| | |
|---|---|
| What | The token program of the KaspaRocket launchpad (kasparocket.fun: on mainnet since 2026-10-20, and on testnet-10): the `KCC20` actor of upstream's published `KCC20PublicMint` app (`../kcc20-reference/KCC20.public-mint.sil`, argent-lang/kcc20-reference `c8a0871`) built with `MAX_DELEGATES = 9` and `MAX_TOKEN_OUTPUTS = 10` (10 token inputs / 10 token outputs per transfer) instead of 2 / 3. Redeem script 11,458 B: a 1-byte Sil prefix, the 145-byte Sil state (`gen__kcc20_template`, then the standard 112-byte KCC-20 state), an 11,312-byte suffix |
| Where | testnet-10 token ZKASB, covenant `fee989eb1e6b9d0355c0f474d473019987c9a4f2919c13d2cdcd9e57bae07c42` (listed by kasparocket.fun `/api/tokens`), read from `api-tn10.kaspa.org`: genesis `e452f3f5ed0c6e853a7bf92bbc2f1e370e628b781611f4f3f2d1bde9bfcba5a2`, the move of the whole supply into the launch's pool `89cddaea3fd8d2c6af311deeba51c1855195c3f992e66f4557cee5ea8bb96548`, the pool's genesis `f49ec210ca9fd7b30710a23873192e4bd88d9ef6338f21e7ff4547eb93f5709f` (LP_AMM covenant `732a7a6d4f1715ee979883979842e3352504d206fbb6a91ffbe8c6e33b1055a8`), the first buy `db9ccdfb872e47a8abbffb664660e6db8c5b2e994e41e7a505875f183f9de48d` (`observed.json`) |
| Source | `KCC20.public-mint.10x10.sil`: the vendored fixture with seven lines changed (the two limits, their two literal bounds each, and `gen__const_kcc20_suffix_len`, the program's own suffix length that argentc embeds as a fixed-width constant: `0x302c0000` = 11,312). `KCC20.public-mint.10x10.ctor.json`: the context value `gen__kcc20_template` = the program's Sil template hash `ff3ae732e3a2819bf9181f5aed6b1e9d0f7abea70a1dd655b8e777cbcbd2422b` (prefix 1 B, suffix 11,312 B), then the example state of `../kcc20-reference/KCC20.public-mint.ctor.json` |
| Build | `scripts/build-contracts.sh` (`THIRD_PARTY`) compiles it with KOB's pinned silverc v1.0.0 into `contracts/artifacts/KaspaRocketToken11458.json`; with the observed state as constructor arguments the bytecode is the observed redeem script byte for byte |
| Template hash | `3d689d6273b7631e69f672d5ca744cb266ae997b04974196439858675cc04fe1` (KCC-1 actor-type handle: prefix 34 B = Sil prefix and context push, open state 112 B, suffix 11,312 B), as for `KCC20PublicMint` |
| Publisher's source | La-Gauffre/KaspaRocket-Silverscript-Contract `86fe1e8e7dc9a89e4852d9c5e28b5b70648cc23b` (2026-10-14, "Keep only the four contracts and rewrite the README"; the file came with `d470c8c`, the same day), `kcc20.sil` (git blob `e436794a`, sha256 `ba4403ed953dbb81023ffcd3723112a423dc23e2d7beeeb04556722d70e66321` with LF line endings): byte for byte `KCC20.public-mint.10x10.sil` here without KOB's five-line header comment. Compiled by the official silverc v1.0.0 release (the binary `contracts/silverc.lock` pins) with `KCC20.public-mint.10x10.ctor.json`, it gives the bytecode of `contracts/artifacts/KaspaRocketToken11458.json` byte for byte (checked 2026-10-27). Authors: La-Gauffre (the repository); the token logic is argentc output of argent-lang/kcc20-reference `c8a0871` |
| Licence | ISC (the upstream program, `../kcc20-reference/UPSTREAM.md`). The KaspaRocket repository has **no licence file** and its README states none: KOB does not vendor its file. KOB builds from its own derivation of the vendored ISC fixture, and the 10 / 10 build is public on-chain bytes |
| Artifact | KaspaRocket publishes **no artifact JSON** for the build (the repository holds four `.sil` files and a README; KCC-20 sections 2-3 ask a covenant with non-default limits to publish one). KOB's artifact is its own build of the source above |

## What KaspaRocket's repository says, and what is on chain

Since `d470c8c` / `86fe1e8` (2026-10-14) the repository's `kcc20.sil` is the deployed program (above). Before that,
La-Gauffre/KaspaRocket-Silverscript-Contract `3966ab2` (2026-10-08) published as `kcc20.sil` the standalone reference
(`contracts/kcc20/KCC20Ref.sil`) with the limits 9 / 10. That source is not the deployed program: compiled with silverc
v1.0.0 it gives a 7,724-byte script (template hash `41a9ee0d...`, prefix 1 B), and the silverc its CI pins
(`SILVERSCRIPT_COMMIT` `9aa70b0d0215e7395e2a95b78472eba0a5b103a5`, 2026-07-15, 61 commits before v1.0.0) cannot parse it
(`entry` declarations); its CI compiled only the three other contracts (`86fe1e8` removed the CI). The launchpad's token UTXOs
carry the public-mint actor at 10 / 10 (above). The repository's `LP_AMM.sil` reads a four-field KRON-style token state; the deployed pool
(41,922-byte redeem script) is a later version: its state adds the reserves, and it validates the pool's token output against
the Sil template `ff3ae732...` with the token's suffix embedded as data.

## How the launch holds tokens

The genesis creates the whole supply (1,000,000,000 base units) in one output owned by the launchpad's key (owner scheme 0);
the next transaction moves it into the pool: one token output owned by the LP_AMM covenant id (owner scheme 4, the program's
`OWNER_COVENANT_ID`: spendable by any transaction holding an input of that covenant, the pool's script then binds the
amounts). A buy spends the pool's token UTXO as the token group's leader (the pool reads the first token input and the first
token output of the token's covenant id) and pays the buyer a scheme-0 holder. The pool is its own covenant, outside the
token's genesis group: the genesis verifies as a fixed supply with no companion. "Graduation" is the pool's irreversible switch
from its bonding curve to a constant-product pool inside the same covenant (`binary_flag` 0 to 1): no token moves, mints or burns
then, and there is no LP token. KaspaRocket ships no issuer actor (`PublicMint`, `TokenSeed`) for this build, and the token actor
creates only holders of its own template, so the supply is fixed at genesis (`minters: genesis-only`, no companions).

## Mainnet census (2026-10-27)

kascov.io's token list (`data/mainnet/tokens.json`, used only for the covenant ids and genesis txids) names 136 mainnet tokens on
this build, all on one version (Sil template `ff3ae732...`, handle `3d689d62...`); 133 are KaspaRocket launches (3 of them
with other genesis payloads: the build names a program, not a venue). Read-only checks, nothing trusted from the list:

* For ROCKET (`7cf4b45ebf8672251da9903ea6fbf1771a9d38096aad4a7d0e32503462ba6b61`, genesis
  `71b6c6af668cd81d859e1121c84580fce4d7434b4dbd520663896267511ebe50`), GUZA (`3cb7bd371ea2014af95f3471d2995ede8ffbde454d3a37e3b566880daf011c5a`,
  graduated) and POKEMONKAS (`592404037e7d5ab2154c3dac9f26da08e94e325d7192a8d20edae1bfe8068086`), from api.kaspa.org: the redeem script the
  genesis output's first spend reveals and a live cell's are 11,458 B, `aa20 blake2b-256(script) 87` is the output's script public
  key, the 34-byte prefix and the 11,312-byte suffix are this artifact's, and the handle hash recomputes to `3d689d62...`.
* `web/scripts/registry-genesis-evidence.mjs` (public mainnet node, v2.1.0, plus api.kaspa.org) over all 136, then
  `kob registry verify-genesis --network mainnet` against a scratch registry listing them: **136 of 136 verified** (txid and covenant id
  recomputed, every redeem script matched to its P2SH hash): one token output each (index 0), 1,000,000,000 base units, no other
  output bound to the token's covenant id, no minter output.
* Decimals: no KaspaRocket genesis declares any (its payload is KaspaRocket's own JSON with name, ticker and image, not KCC-23);
  the launchpad shows 3. KOB uses 3 for a token of this program that declares none: the profile's `default_decimals`, source
  "KaspaRocket convention" (a token declaring decimals under KCC-23 keeps its own).

## In KOB

Embedded as `KaspaRocketToken11458` (`crates/kob-protocol/data/programs.json`): `reviewed` (internal review, maintainer decision
2026-10-27), `networks: [mainnet, testnet-10]`, on the shipped registry's strict list (`kasparocket-token-11458`), so a mainnet
build trades its tokens without a registry entry (each carries `genesis_unverified` until one records its genesis check). It is a
KCC-20 codec program (custody 4, holder 0, witness `kcc20-transfer`): the KCC-20 order kinds serve it unchanged. A
transaction spending KOB orders carries at most 8 token inputs of one token (the orders' stray scan,
`MAX_TOK_IN_KCC20`); the program's own limit is 10. Measured facts and the review items with a test:
`crates/kob-tests/tests/program_kasparockettoken11458.rs`; the order grid, the pair attacks and the nine-input refusal on its
holders: `kcc20_public_mint_tests.rs`, `audit_pair_sec_tests.rs`, the pair suites (`pair_harness::mixed_pairs_kr`),
`kob_ifd_pair_tests.rs` NIS8 / NIS9; the executor: `crates/kob-executor/tests/flow_kasparocket.rs`; the review:
`docs/spec/token-programs.md` section 6.

## KOB on testnet-10 with this program (2026-10-14)

A token of KOB's own issued with this program and traded with KOB's CLI (`kob-cli` of this branch, node
`ws://65.108.107.30:18210`, a local indexer from `--start sink`, a local testnet-10 registry that lists the token and,
for that test only, marks the template `reviewed` so that the trading commands take it; the profile stays `pending-review`).
The token input of every transaction below carries the same 34-byte prefix (context `ff3ae732...`) and the same 11,312-byte
suffix as the launchpad's own tokens: template hash `3d689d6273b7631e69f672d5ca744cb266ae997b04974196439858675cc04fe1`.
Every transaction was accepted.

| Step | Command | Transaction |
|---|---|---|
| Genesis of KOBKR (1,000,000.000, 3 decimals, one holder: the issuing key), covenant `9983ad3b4ebdc6be73ca7c44f52e356d6579a53a29f9cc88d7b1fa0a2e99ff93` | `kob token issue --program kasparocket-testnet --network testnet-10` | `cc72831da9adef2c9e7bb5ba0eee9bd2ca143a6f6581f89e722d210cccc3f65c` |
| 100 KAS to the second key | `kob send KAS` | `41f32f5854d3e683be3a546d07dc4bf23facccf2cc83f293da086c325e6ab8c9` |
| Plain transfer: 1,000 KOBKR to the second key (the genesis output, a scheme-0 leader with its signature) | `kob send KOBKR` | `69d25b8c8c58e89b454a8fe87b121ddb325fbaebbbce6d79c1dfa48c390cc8ad` |
| `KobAsk` placed: 1,000 KOBKR at 0.05 KAS, minimum fill 100 (order `131c2c8c7aa45277e59fd7b5893b25c31a52073058a5a787d828f4f62eb9a874`) | `kob place limit --side sell` | `99ba137823dc22ef6486fa36bc90b4510739e1fcadfca4dc1b80597667b647a0` |
| Partial fill: the second key buys 300 KOBKR (15 KAS to the maker, 700 left in custody) | `kob take --amount 300KOBKR` | `e7b2d9f23e7c2e69055c36ca1602a2e679194f18afc3ba69181829d3a6146ea1` |
| Cancel: the 700 KOBKR custody back to the maker | `kob cancel` | `3dfbf0936033ae9135f4615dba2be6aa55b647408c1c93e3efa0e0a4011d057f` |
