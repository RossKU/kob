# Kozmic (third-party, observed on chain): ZK-bridge tokens of `KCC20PublicMint`, Kaspa mainnet

| | |
|---|---|
| What | Kozmic, Kaspero Labs' USDC bridge (github.com/kasperolabs/kozmic-bridge, commit `87dca0e`, BSL 1.1, read only). Its tokens' holders are the `KCC20` actor of upstream's published `KCC20PublicMint` app, byte for byte (KOB's `kcc20-ref-public-mint`, template `734850b0...`). The genesis group of each token holds one `BridgeMint` (`bridge-v7/BridgeMint.sil`), the only issuer. It mints only in a transaction that spends the bridge covenant its state names: a RISC Zero light client of Ethereum that proves deposits into a USDC vault and decides the amounts and owners. A burn is a transfer to an unspendable owner |
| Pin | One `BridgeMint` build, the `zk-bridge-minter` companion of `KCC20PublicMint` (`crates/kob-protocol/data/programs.json`), by its template hash over the KCC-1 actor-type cut: `92c69c7f4f652fed2e66f2b4ac402b9725df21ecc268c1bedd8d69950c0f28e0`, prefix 34 B (the 1-byte Sil prefix `6b` and the push of `gen__kcc20_template`, `9703112e...`, the app's `KCC20`, so the pinned bytes mint only that program), state 99 B, suffix 1,340 B. Sil template hash (prefix 1 B) `dc96afcde4874e0750e85ecffe91a845365ff16d952a4c1a92f49f7a0da70102`, the `minter_template` of the tokens' KCC-23 declarations |
| Fixture | `observed.json`: for each token below, the genesis group (one output), the KCC-23 binding that names its authorising outpoint, and the `BridgeMint` redeem script its first spend revealed; and a kzUSDC cell at the burn owner. Read once on 2026-11-17 from kascov.io's per-token files and KOB's own mainnet indexer (read only). Public chain data; KOB vendors no source of kozmic-bridge |
| Review | None. KOB does not review the bridge or the minter: the pin only recognises the minter's code (section "In KOB") |

## Tokens (mainnet)

| Covenant id | Genesis | Bridge covenant (the `BridgeMint` state) | What |
|---|---|---|---|
| `cc9957fe0d485f79994ed583aa7e16bacf194d9b9d6d77aeedc129349ddad12c` | `1f6cc7a9...` | `bf6901190501d55274375960c4f119f6b5e8cbc933192838f4a1ffaed85b7158` | kzUSDC, instance 2 (the issuer's current one) |
| `9753d77b8cf60b6349786ca57a9ab89c9adf8bc980c4b4e804f3ac05c775cfe2` | `b3b20712...` | `054803b64f7625211ea76792c3f6d63296aed6e6420804cc33ffea4625294708` | "Kozmic Bridged USDC" (ticker `kzUSDC`), instance 1, closed to deposits |
| `13af6019a5c09f7afc5814b065253c6468ed59dc0f010a329d108d10719ad58d` | `f935d5e7...` | `89d8a40758c34a897c5383cb91b12064294007f9d8dd3a18fe70162fec7a7cdd` | Sepia.k, the rehearsal backed by Sepolia test USDC |

All three run the same `BridgeMint` build and so all three verify: a ticker or a verified genesis does not tell them apart, only
the covenant id does.

## What KOB checked

* **The cut.** The redeem script of genesis output 1 of kzUSDC and of instance 1, as each one's first spend revealed it (kzUSDC:
  `8e780571...`), hashes to the output (`p2sh/v1`). Its Sil template hash is `dc96afcd...`, the issuer's published value, and the
  template hash of the KCC-1 cut is the pin. The two scripts differ only in the bridge covenant id. Sepia.k's script was not in the
  source read; kzUSDC's bytes with Sepia.k's declared bridge covenant id hash to its genesis output, which proves it is the same build.
* **The state** (99 B after the prefix, three 32-byte pushes): `gen__bridge_mint_template` (the minter's own Sil template,
  `dc96afcd...`, not used by its code), `bridge_covenant_id` (each token's declared bridge) and `extension_commitment` (zero; the
  last field, `kob_protocol::kozmic::bridge_mint_extension`).
* **The genesis.** Each group recomputes to its covenant id and verifies under `KCC20PublicMint` with the pin: supply 0, the
  `BridgeMint` a mint authority without an allowance. Without the pin it fails as contaminated. A `BridgeMint` with any byte of its
  code changed, or whose mints would carry another class than the token's, is refused (`kozmic_bridge_tests.rs`).
* **Burns.** The burn owner is `d379e50f3e2203f9a92584916335bb7cd014b237ceaa8b36f1d3d80d0cc88b32` (owner scheme 3), the constant
  `BURN` of the issuer's `bin/minter-artifact.py` and `bridge-v7/src/lib.rs`. KOB computed it: it is `Blake2b(6a)`, the `p2sh/v1`
  authority of the script `OP_RETURN` alone. A holder of scheme 3 moves only with an input that reveals a script hashing to its
  owner; the only such script known fails at once, so the cell can never move (the same standard as kUSDT's burns). kzUSDC cell
  `25dac4a9...:0` (10 kzUSDC) is one.

## What the pin does not prove

The pin recognises the minter's code. The bridge it trusts is in its state, so anyone can deploy the same build pointing at a
covenant of their own that mints freely, and that token's genesis verifies the same. Checking the bridge from Kaspa data was
considered and not done:

* the bridge covenant's script carries its parameters (the Ethereum vault address, the guest's image id, the fee and mint limits)
  in its code: instance 1 and 2 differ only in the 20-byte vault address, so a pin of the bridge is a pin of one instance, and
  every new instance would need a KOB release;
* even with the bridge pinned, Kaspa data cannot show that the vault is the real `UsdcVaultV4` holding USDC, or that the bridge's
  initial Ethereum checkpoint (its genesis state: header and sync committee) is genuine.

So the web app names the issuance "ZK bridge (Kozmic BridgeMint)", says that the bridge covenant and its Ethereum vault are not
verified by KOB, and shows the bridge covenant id the minter names, for a holder to compare with the issuer's.

## In KOB

* `crates/kob-protocol/data/programs.json`: the `zk-bridge-minter` companion of `KCC20PublicMint` (opaque: recognised by its
  template hash; its state read for the extension commitment and the bridge covenant id, `kob_protocol::kozmic`).
* `crates/kob-protocol/src/registry.rs`: the genesis check lists it among the minters and reports the bridge covenant id
  (`CompanionOutput::bridge`); kob-wasm passes it to the web app (`companionOutputs[].bridge.bridgeCovenantId`).
* `crates/kobd/src/indexer/record.rs`: an output at the burn owner is not a holding.
* Nothing of the token is in `registry/tokens.json`: KOB does no token curation.
