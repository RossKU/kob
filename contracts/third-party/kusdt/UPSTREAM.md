# kUSDT (third-party, observed on chain): a federated-bridge token of `KCC20PublicMint`, testnet-10

| | |
|---|---|
| What | kUSDT (kaspador.github.io/kusdt-docs): a KCC-20 token whose holders are the `KCC20` actor of upstream's published `KCC20PublicMint` app, byte for byte (KOB's `kcc20-ref-public-mint`, template `734850b0...`), and whose issuance is a federated bridge. Its genesis group holds a `BridgeMint` (the only issuer: it mints kUSDT against USDT locked in an EVM vault when 3 of its 5 signers sign; a guardian key can pause it) and a `TokenSeed` of the app's `27f8403` build. A redemption is a KCC-20 transfer to a burn owner that names the payout chain and address |
| Pin | Two `BridgeMint` builds, each a `bridge-minter` companion of `KCC20PublicMint` (`crates/kob-protocol/data/programs.json`), each by its template hash over the KCC-1 actor-type cut (prefix 34 B = the 1-byte Sil prefix and the push of the compiler-owned `gen__kcc20_template`, which is `9703112e...`, the app's `KCC20`, so the pinned bytes mint only that program). The first build: `15e5532d4115550df60b7f9b1af2a0c996463cc4ba243bcdf9c478174fb6da40`, state 173 B, suffix 4,151 B, Sil template hash (prefix 1 B) `11ae39c524e45cfe477e53dd15715e35dd61a241c864a6537d1d4c509649cc25`. The build with the signer-recovery path (section "The recovery build"): `d8b48be4cf93156a40f0ac443da5eddf293e79b862a89c2e1c96b21d8c4a3446`, state 250 B, suffix 8,215 B, Sil template hash `1bcabab63c4906ab3cb08a7233bf18cb813b248450fc400538c89d233f014964` |
| Fixture | `observed.json`: the testnet-10 genesis transaction (BridgeMint, TokenSeed, the creator's change), the `BridgeMint` redeem script its first spend revealed, and a redemption with its payload and burn output; under `recovery_build` the same (no redemption) for the deployment that superseded it. Public chain data; KOB vendors no source of kUSDT |
| Review | None. KOB does not review the bridge: the pin only recognises the covenant (section "In KOB"). Everything below about what the bridge does is the issuer's description |

## Deployments (testnet-10)

| Covenant id | Genesis | `BridgeMint` | State |
|---|---|---|---|
| `6711958e2bc51c590426037020d9683ebd5daf42593de1797b65d25b92566314` | `73e7a514...` | first build (`15e5532d...`) | superseded: a test deployment of an earlier release, vaults `0x7a9CA592...7C57` |
| `a1d9fd1347128011ad0036b077949d63274d5d3b8ac8351776c30c945ef2c2fb` | `7ecb75d0701a3c2d3735322a93ceb349d60ba69a6a617fe229ba2a66d50ed207` | recovery build (`d8b48be4...`) | the issuer's current record (`deployments/testnet.json`, testnet window 2026-11-04 to 14); KasPool pool id `99445008e36139d0a57040f562414e298a3022fb739c0e9374b1dbb9f4c42171` |

The issuer says every code change gives a new covenant id: the two are different tokens, linked only by redeeming out of the old one
on the EVM side. Neither is a mainnet kUSDT.

## The recovery build (2026-11-15)

Source: github.com/kaspador/kusdt-open, commit `671bebe98cd7916108a33d7f4a46dd1ea1c55e5a` (release `mainnet-v0.9.8-rc.25`),
`bridge/kaspa/contracts/bridge_mint.ag` and `SPEC.md` section 6, read only; nothing of it was built or run.

* **What changed.** Five state fields follow the extension commitment, which is no longer the last field: `recovery_delay`,
  `recovery_nonce`, `recovery_pending`, `recovery_signer_set`, `recovery_guardian`. The commitment is bytes 131..164 of the 250-byte
  span (`kob_protocol::kusdt::bridge_mint_extension` reads each build by the span's length and refuses any other). The mint and govern
  digests sign the minter UTXO's age, and every input of a spend must be settled (`require_settled_inputs`).
* **Recovery.** Three of the five signers (the mint quorum) can propose a new signer set and guardian (`govern` action 6). Anyone can
  carry it out (`execute_recovery`, no signature) once the minter UTXO has stood untouched for `recovery_delay` DAA: 36,000 DAA
  (about 1 hour) on this deployment, 2,592,000 DAA (72 hours) planned for mainnet, fixed at genesis. Until then any one signer can
  veto it (action 7), and every spend of the minter restarts the wait; four signers replacing the set (`set_signers`) end it. So
  three keys can take over the signer set and guardian if no honest signer vetoes in time. The issuer's README does not mention it;
  SPEC section 6 does.
* **What KOB checked.** The redeem script of genesis output 0 as its first spend (`efc13cf3...`) revealed it equals the issuer's
  published artifact (`bridge/kaspa/artifacts/kusdt.json`, `BridgeMint`) outside the state span, and its state is the issuer's
  `launch.initialMinter.state`; the pin `d8b48be4...` is the template hash of that cut, computed by KOB
  (`silverscript_abi::template_hash`). The genesis group (`7ecb75d0...`) recomputes to `a1d9fd13...` and verifies under
  `KCC20PublicMint` with it (`kusdt_bridge_tests.rs`); without it, or with only the first build's pin, it fails as contaminated.
  The holder program is unchanged (`KCC20` `9703112e...`, the same `TokenSeed`).

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
  (`crates/kob-tests/tests/programs/kusdt_bridge_tests.rs`). Without the pin the same group fails as contaminated.
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
* the mainnet `BridgeMint` is a pinned build (its cut hashes to a pinned handle; a new release is a new pin), the token's genesis
  verifies with it, and its `recovery_delay` and limits are recorded from the genesis state.

## In KOB

* `crates/kob-protocol/data/programs.json`: the `bridge-minter` companions of `KCC20PublicMint`, one per build (opaque: recognised by its template hash,
  its state not read except for its extension commitment, `kob_protocol::kusdt::bridge_mint_extension`, held to the token's one
  class). A genesis holding it verifies; the report lists it among `minter_outputs` (a mint authority: the supply is not fixed) and in
  `companion_outputs` with its role, and it adds nothing to `mint_allowance` (not a public mint).
* The web app: a token whose genesis check finds the companion shows "Issuance: Federated bridge (BridgeMint)", per the issuer's
  docs, and a collapsed note on bridge custody; it counts no public mint for it.
* The indexer: an output a redemption payload names and whose proven state is that burn owner (`kob_protocol::kusdt::is_burned`)
  is not a holding (`GET /v1/token-utxos` does not list it).

## Licence

kusdt-open is under the MIT licence; KOB keeps chain bytes of it (the revealed `BridgeMint` redeem scripts in `observed.json`) and
hashes, no source. Its notice:

```
MIT License

Copyright (c) 2026 Trestly

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
