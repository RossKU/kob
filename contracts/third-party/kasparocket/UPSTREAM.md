# KaspaRocket token program (third-party, observed on chain): `KaspaRocketToken11458`, testnet-10, pending review

| | |
|---|---|
| What | The token program of the KaspaRocket launchpad (kasparocket.fun, in beta on testnet-10): the `KCC20` actor of upstream's published `KCC20PublicMint` app (`../kcc20-reference/KCC20.public-mint.sil`, argent-lang/kcc20-reference `c8a0871`) built with `MAX_DELEGATES = 9` and `MAX_TOKEN_OUTPUTS = 10` (10 token inputs / 10 token outputs per transfer) instead of 2 / 3. Redeem script 11,458 B: a 1-byte Sil prefix, the 145-byte Sil state (`gen__kcc20_template`, then the standard 112-byte KCC-20 state), an 11,312-byte suffix |
| Where | testnet-10 token ZKASB, covenant `fee989eb1e6b9d0355c0f474d473019987c9a4f2919c13d2cdcd9e57bae07c42` (listed by kasparocket.fun `/api/tokens`), read from `api-tn10.kaspa.org`: genesis `e452f3f5ed0c6e853a7bf92bbc2f1e370e628b781611f4f3f2d1bde9bfcba5a2`, the move of the whole supply into the launch's pool `89cddaea3fd8d2c6af311deeba51c1855195c3f992e66f4557cee5ea8bb96548`, the pool's genesis `f49ec210ca9fd7b30710a23873192e4bd88d9ef6338f21e7ff4547eb93f5709f` (LP_AMM covenant `732a7a6d4f1715ee979883979842e3352504d206fbb6a91ffbe8c6e33b1055a8`), the first buy `db9ccdfb872e47a8abbffb664660e6db8c5b2e994e41e7a505875f183f9de48d` (`observed.json`) |
| Source | `KCC20.public-mint.10x10.sil`: the vendored fixture with seven lines changed (the two limits, their two literal bounds each, and `gen__const_kcc20_suffix_len`, the program's own suffix length that argentc embeds as a fixed-width constant: `0x302c0000` = 11,312). `KCC20.public-mint.10x10.ctor.json`: the context value `gen__kcc20_template` = the program's Sil template hash `ff3ae732e3a2819bf9181f5aed6b1e9d0f7abea70a1dd655b8e777cbcbd2422b` (prefix 1 B, suffix 11,312 B), then the example state of `../kcc20-reference/KCC20.public-mint.ctor.json` |
| Build | `scripts/build-contracts.sh` (`THIRD_PARTY`) compiles it with KOB's pinned silverc v1.0.0 into `contracts/artifacts/KaspaRocketToken11458.json`; with the observed state as constructor arguments the bytecode is the observed redeem script byte for byte |
| Template hash | `3d689d6273b7631e69f672d5ca744cb266ae997b04974196439858675cc04fe1` (KCC-1 actor-type handle: prefix 34 B = Sil prefix and context push, open state 112 B, suffix 11,312 B), as for `KCC20PublicMint` |
| Licence | ISC (the upstream program, `../kcc20-reference/UPSTREAM.md`); the 10 / 10 build is public on-chain bytes |

## What KaspaRocket's repository says, and what is on chain

La-Gauffre/KaspaRocket-Silverscript-Contract (`3966ab2`, 2026-10-08) publishes `kcc20.sil`: the standalone reference
(`contracts/kcc20/KCC20Ref.sil`) with the limits 9 / 10. That source is not the deployed program: compiled with silverc
v1.0.0 it gives a 7,724-byte script (template hash `41a9ee0d...`, prefix 1 B), and the silverc its CI pins
(`SILVERSCRIPT_COMMIT` `9aa70b0d0215e7395e2a95b78472eba0a5b103a5`, 2026-07-15, 61 commits before v1.0.0) cannot parse it
(`entry` declarations); its CI compiles only the three other contracts. The launchpad's token UTXOs carry the public-mint
actor at 10 / 10 (above). The repository's `LP_AMM.sil` reads a four-field KRON-style token state; the deployed pool
(41,922-byte redeem script) is a later version: its state adds the reserves, and it validates the pool's token output against
the Sil template `ff3ae732...` with the token's suffix embedded as data.

## How the launch holds tokens

The genesis creates the whole supply (1,000,000,000 base units) in one output owned by the launchpad's key (owner scheme 0);
the next transaction moves it into the pool: one token output owned by the LP_AMM covenant id (owner scheme 4, the program's
`OWNER_COVENANT_ID`: spendable by any transaction holding an input of that covenant, the pool's script then binds the
amounts). A buy spends the pool's token UTXO as the token group's leader (the pool reads the first token input and the first
token output of the token's covenant id) and pays the buyer a scheme-0 holder. The pool is its own covenant, outside the
token's genesis group: the genesis verifies as a fixed supply with no companion.

## In KOB

Embedded as `KaspaRocketToken11458` (`crates/kob-protocol/data/programs.json`): `pending-review`, `networks: [testnet-10]`, so
it is on no registry shipped with KOB (the shipped registry is mainnet's) and a mainnet build never trades it. It is a
KCC-20 codec program (custody 4, holder 0, witness `kcc20-transfer`): the KCC-20 order kinds serve it unchanged. A
transaction spending KOB orders carries at most 8 token inputs of one token (the orders' stray scan,
`MAX_TOK_IN_KCC20`); the program's own limit is 10. Measured facts and the review items with a test:
`crates/kob-tests/tests/program_kasparockettoken11458.rs`; what is still open: `docs/spec/token-programs.md` section 6.

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
