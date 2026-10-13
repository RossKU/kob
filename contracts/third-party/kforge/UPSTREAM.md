# KForge token program (third-party, observed on chain): dry-run fixture, NOT embedded

| | |
|---|---|
| What | `program.bin`: the redeem script (1,785 bytes) of a KForge launchpad token UTXO: a one-byte prefix (`OP_TOALTSTACK`, an entry selector), the KRON-layout 46-byte state, a 1,738-byte suffix |
| Where | testnet-10 transaction `b33e910b4bd605f1a1638ba6e4053a486d146f674d1c98b7390a5094acef3c06`, input 1 (token covenant `38af6574...`, KForge token "ONEBUY", listed by `api-test.kforge.xyz`), read from `api-tn10.kaspa.org` |
| Template hash | `5b136633944a2371a1d0f6a94fdc0d329327ffc700fdec097fc59e76e1d33ae1` (blake3 over prefix 1 B and suffix 1,738 B) |
| Licence | none published; public on-chain bytes, kept as a test fixture like the KRON programs |
| `observed-transfer.json` | that input's signature script and the transaction's token outputs |
| `profile.candidate.json` | the profile `kob registry add-program` derives from `program.bin`, plus the owners, slots and carrier floor measured in the engine |

KOB does not embed this program: it is not in `crates/kob-protocol/data/programs.json`, has no registry entry and no token
on it is tradable. `crates/kob-tests/tests/program_kforge_dry_run.rs` is the onboarding dry run; its result (the program
refuses address-presence holders, which KOB's KRON order templates deliver to) is in `docs/spec/token-programs.md` section 6.
