# kUSDT (third-party, observed on chain): a federated-bridge token of `KCC20PublicMint`, testnet-10

| | |
|---|---|
| What | kUSDT (kaspador.github.io/kusdt-docs): a KCC-20 token whose holders are the `KCC20` actor of upstream's published `KCC20PublicMint` app, byte for byte (KOB's `kcc20-ref-public-mint`, template `734850b0...`), and whose issuance is a federated bridge. Its genesis group holds a `BridgeMint` (the only issuer: it mints kUSDT against USDT locked in an EVM vault when 3 of its 5 signers sign; a guardian key can pause it) and a `TokenSeed` of the app's `27f8403` build. A redemption is a KCC-20 transfer to a burn owner that names the payout chain and address |
| Pin | The `BridgeMint` as the `bridge-minter` companion of `KCC20PublicMint` (`crates/kob-protocol/data/programs.json`): template hash `15e5532d4115550df60b7f9b1af2a0c996463cc4ba243bcdf9c478174fb6da40` over the KCC-1 actor-type cut (prefix 34 B = the 1-byte Sil prefix and the push of the compiler-owned `gen__kcc20_template`, which is `9703112e...`, the app's `KCC20`, so the pinned bytes mint only that program; state 173 B; suffix 4,151 B). Its Sil template hash (prefix 1 B) is `11ae39c524e45cfe477e53dd15715e35dd61a241c864a6537d1d4c509649cc25` |
| Fixture | `observed.json`: the testnet-10 genesis transaction (BridgeMint, TokenSeed, the creator's change), the `BridgeMint` redeem script its first spend revealed, and a redemption with its payload and burn output. Public chain data; KOB vendors no source of kUSDT |
| Review | None. KOB does not review the bridge: the pin only recognises the covenant (section "In KOB"). Everything below about what the bridge does is the issuer's description |

## What the issuer publishes (2026-11-05)

The kUSDT documentation (`contracts/bridge-mint`, `contracts/kusdt-vault`, `security/trust-model`, `protocol/burn-owner-and-payload`)
describes:

* **BridgeMint** (`bridge/kaspa/contracts/bridge_mint.ag`): exactly one UTXO, every spend creates its successor. A mint carries 1 to 3
  consecutive deposit nonces of one chain (Ethereum `01`, BNB Chain `02`), needs 3 of the 5 signers over a digest naming the covenant,
  the chain, the nonces, every recipient and amount, and draws on an allowance that refills over DAA score up to a ceiling, under a hard
  cap (`remaining`, 10^15 base units at genesis) that only falls. Pausing needs the guardian; unpausing, limits, the signer set and a
  skipped nonce need 4 of 5.
* **KusdtVault** (one per EVM chain; on testnet Sepolia and BSC testnet at `0x7a9CA592adc5fC024660AF66e44051145a1f7C57`): holds the
  USDT that backs kUSDT; pays a release only with 3 signer signatures, within a daily release allowance. Its **owner** sets the fees and
  limits within hard bounds and can replace all five signers (`setSigners`) and raise the release limits: with a signer set of its own,
  it can release the whole backing. For mainnet the issuer promises an owner that is a Safe behind a 48-hour timelock (its deploy script
  refuses a non-contract owner on chain ids 1 and 56); the timelock is the holders' window to redeem before such a change takes effect.
  On testnet the owner is a single key (`0x4f6d...b1be`), which matches that plan for a test deployment.
* **Burns.** `R(chain, address) = 6a 0f "kUSDT/redeem/v1" 01 chain 14 address[20]` (40 bytes, `6a` = `OP_RETURN`); the burn owner is
  owner scheme 3 (`p2sh/v1`) with owner `Blake2b(R)`. No input can satisfy a script that starts with `OP_RETURN`, so tokens sent there
  never move again. A redemption's payload names its burns: `"KUSDT" 01 count (output_index chain address[20]){count}`.

## What KOB checked

* The redeem script of the observed `BridgeMint` spends: the 1-byte Sil prefix, a 206-byte state that starts with the
  `gen__kcc20_template` push of the app's `KCC20` Sil hash (`9703112e...`) and ends with the push of its extension commitment (zero), and a
  4,151-byte suffix; the first spend (`85f684bd...`, in `observed.json`) and the latest one read (2026-11-05) carry the same prefix and
  suffix, which are also what the issuer's published wasm core (`@kusdt/core` `layout`) reports for `BridgeMint`.
* The genesis group (`73e7a514...`) recomputes to the token's covenant id `6711958e...` and verifies under `KCC20PublicMint` with the
  `BridgeMint` as its `bridge-minter` companion and the unspent `TokenSeed` rebuilt from the creator's key
  (`crates/kob-tests/tests/kusdt_bridge_tests.rs`). Without the pin the same group fails as contaminated.
* The issuer's worked burn example (owner `09d6acad...`) and the observed redemption (`929bf07b...`): its output 0 is the
  `KCC20PublicMint` P2SH of a 20,000,000-unit state at the burn owner its payload names.

## Trust: what KOB does not check

The bridge is a federation. Whatever a `BridgeMint` mints is real kUSDT to every program and order: a bad quorum of 3 signers mints
kUSDT that looks the same. The backing (USDT on Ethereum and BNB Chain) sits in the vaults, controlled by the signers and the vault
owner: if they misbehave or lose their keys, the backing, and with it the token's value, can be lost. KOB checks none of it (deposits,
the vaults' balances, the signer set, the owner); the web app says so in a note on the token's page.

## Before KOB lists a mainnet kUSDT

A listing (a registry entry, a curated list KOB pins) of a mainnet kUSDT waits for these checks on chain, recorded with the evidence:

* the vault on each EVM chain names as `owner()` a contract that is the Safe the issuer promises, behind a timelock whose minimum
  delay is at least 48 hours, with no `pendingOwner()`; a single key as owner (as on testnet) is not listable, because the owner alone
  can replace all signers and raise the release limits, that is, drain the vault, and the timelock is the holders' only exit window;
* the mainnet `BridgeMint` is the pinned bytes (its cut hashes to `15e5532d...`) and the token's genesis verifies with it.

## In KOB

* `crates/kob-protocol/data/programs.json`: the `bridge-minter` companion of `KCC20PublicMint` (opaque: recognised by its template hash,
  its state not read except for its extension commitment, `kob_protocol::kusdt::bridge_mint_extension`, held to the token's one
  class). A genesis holding it verifies; the report lists it among `minter_outputs` (a mint authority: the supply is not fixed) and in
  `companion_outputs` with its role, and it adds nothing to `mint_allowance` (not a public mint).
* The web app: a token whose genesis check finds the companion shows "Issuance: Federated bridge (BridgeMint)", per the issuer's
  docs, and a collapsed note on bridge custody; it counts no public mint for it.
* The indexer: an output a redemption payload names and whose proven state is that burn owner (`kob_protocol::kusdt::is_burned`)
  is not a holding (`GET /v1/token-utxos` does not list it).
