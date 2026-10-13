# KForge token program (third-party, observed on chain): `KForgeToken1785`, testnet-10, pending review

| | |
|---|---|
| What | `program.bin`: the redeem script (1,785 bytes) of a KForge launchpad token UTXO: a one-byte prefix (`OP_TOALTSTACK`, an entry selector), the KRON-layout 46-byte state, a 1,738-byte suffix |
| Where | testnet-10 transaction `b33e910b4bd605f1a1638ba6e4053a486d146f674d1c98b7390a5094acef3c06`, input 1 (token covenant `38af6574...`, KForge token "ONEBUY", listed by `api-test.kforge.xyz`), read from `api-tn10.kaspa.org` |
| Template hash | `5b136633944a2371a1d0f6a94fdc0d329327ffc700fdec097fc59e76e1d33ae1` (blake3 over prefix 1 B and suffix 1,738 B) |
| Licence | none published; public on-chain bytes, kept as a pinned program like the KRON programs |
| `observed-transfer.json` | that input's signature script and the transaction's token outputs |

KOB embeds this program as `KForgeToken1785` (`crates/kob-protocol/data/programs.json`, written by
`kob registry add-program contracts/third-party/kforge/program.bin --name KForgeToken1785 --slots 1/3 --holder 0 --network testnet-10`):
`pending-review`, `networks: [testnet-10]`, so it is on no registry shipped with KOB (the shipped registry is mainnet's) and a
mainnet build never trades it; a testnet-10 build trades its tokens through the delivery id_type 0 builds of the KRON order
kinds (`KobAskKronD0` .. `KobIfdAskKronD0`, `contracts/adapters/kron/README.md`), because the program refuses address-presence
holders (id_type 3) and authorises id_type 0 holders by a token-level signature. Measured facts and the review items with a test:
`crates/kob-tests/tests/program_kforgetoken1785.rs`; what is still open: `docs/spec/token-programs.md` section 6.
