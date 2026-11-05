# KForge token program (third-party, observed on chain): `KForgeToken7214`, mainnet and testnet-10, reviewed

| | |
|---|---|
| What | The token program of the KForge launchpad (kforge.xyz: a bonding curve, then an AMM pool, on mainnet and testnet-10): the `KCC20` actor of upstream's published `KCC20PublicMint` app (`../kcc20-reference/KCC20.public-mint.sil`, argent-lang/kcc20-reference; the token program is the same at `c8a0871` and `27f8403`) built with `MAX_DELEGATES = 5` and `MAX_TOKEN_OUTPUTS = 6` (6 token inputs / 6 token outputs per transfer) instead of 2 / 3. Redeem script 7,214 B: a 1-byte Sil prefix, the 145-byte Sil state (`gen__kcc20_template`, then the standard 112-byte KCC-20 state), a 7,068-byte suffix |
| Source | `KCC20.public-mint.6x6.sil`: the vendored fixture with seven lines changed (the two limits, their two literal bounds each, and `gen__const_kcc20_suffix_len`, the program's own suffix length that argentc embeds as a fixed-width constant: `0x9c1b0000` = 7,068). `KCC20.public-mint.6x6.ctor.json`: the context value `gen__kcc20_template` = the program's Sil template hash `3c1a50add31375a61341f0e041c5eb35ba122684894d37fea8270441088c8564` (prefix 1 B, suffix 7,068 B), then the example state of `../kcc20-reference/KCC20.public-mint.ctor.json` |
| Build | `scripts/build-contracts.sh` (`THIRD_PARTY`) compiles it with KOB's pinned silverc v1.0.0 into `contracts/artifacts/KForgeToken7214.json`; with the observed state as constructor arguments the bytecode is the observed redeem script byte for byte |
| Template hash | `da96d352c404ad7835bc9857b9ae54ae634f41f83a837a5c913ab58cb9f0dec7` (KCC-1 actor-type handle: prefix 34 B = Sil prefix and context push, open state 112 B, suffix 7,068 B), as for `KCC20PublicMint` |
| Licence | ISC (the upstream program, `../kcc20-reference/UPSTREAM.md`). KForge publishes no source repository; KOB vendors nothing of KForge's: it builds from its own derivation of the vendored ISC fixture, and the 6 / 6 build is public on-chain bytes |

## What KForge publishes, and how it was checked (2026-11-05)

KForge's registry (`https://api.kforge.xyz/v6/registry` and `https://api-test.kforge.xyz/v6/registry`, `version` `v6`; sha256 of the
responses `6b52b762...` mainnet, `7a33707a...` testnet-10) names the token program as `argent-lang/kcc20-reference` with
`maxTokenInputs` 6, `maxTokenOutputs` 6, dispatch tags `79c71c23` (`transfer`) and `fd3ef14a` (`transfer_delegator`), Sil template hash
(BLAKE3) `3c1a50ad...`, the source `kcc20_argent.sil` by its blake2b-256 `354841165fb93eea8c7114a70534ad5f7bc7f4143ba3c08fe4a7cc11c2c28d26`,
compiled with `kaspanet/silverscript` `9aa70b0d`. It publishes the app's artifact at `/kcc20/artifact` (the same 144,025 bytes on both
networks, sha256 `9136bfad313649a8d0d52d64534af0507218a60ffd4bf3cf310b60efee3788fb`, app `KCC20PublicMint`, artifact id `0647b1ff...`).

* **Upstream's own build.** argent-lang/kcc20-reference `27f8403` with the two constants of `contracts/kcc20.ag` set to 5 / 6
  (`MAX_DELEGATES`, `MAX_TOKEN_OUTPUTS`; nothing else changed), `cargo +1.94.1 run --locked --example build_contracts` (argentc
  `9a9f4b1`): the generated `fixtures/public-mint/artifact.json` equals KForge's artifact in every field (the artifact id, the three
  actors' bytecode, template hashes and handles, the ABI) except the `root` / `modules` path strings (absolute paths of the build
  machine against KForge's relative ones). Against upstream's unmodified `27f8403` artifact, KForge's differs only in what the limits
  change: the `KCC20` bytecode and handle, the cardinalities 5 / 6 of `transfer`, and the `PublicMint` / `TokenSeed` handle prefixes
  (they embed the `KCC20` template hash); the issuers' own bytecode is `27f8403`'s.
* **The source.** The generated `fixtures/public-mint/sil/KCC20.sil` of that build is byte for byte `KCC20.public-mint.6x6.sil` here
  without KOB's five-line header, and its blake2b-256 is the `354841165fb9...` of KForge's registry
  (`program_kforgetoken7214.rs` recomputes it).
* **KOB's compiler.** silverc v1.0.0 (`contracts/silverc.lock`) compiles that source to KForge's `KCC20` bytecode byte for byte
  (7,214 B, Sil template hash `3c1a50ad...`), and the handle cut of the result is KForge's (`da96d352...`). KForge's registry names
  silverscript `9aa70b0d` as its compiler; KOB did not build that commit (the bytes are reproduced by v1.0.0 and by upstream's
  pinned argentc).

## On chain

Read-only, from the public nodes KOB's soak indexer uses (testnet-10: `wss://boson-10.kaspa.red`; mainnet: the public resolver),
about 90 RPC calls per transaction located (descending the block headers' parents to the DAA score of a UTXO, then `getBlocks`
with transactions); fixture `observed.json`.

* **testnet-10, TESTCAT** (token covenant `f4fef7f80f1560e90c1c567fe1301b19fae203a9b1e89c06c0570a2702fa9e3d`): the genesis
  `e5f8c5a9a75dd3c0d5482b015703f5e29a764455dc3833a4713cc1560a5524c8` has one input (the creator's key) and one output bound to the
  token's covenant id: 1,000,000,000 base units owned by the creator's key (owner scheme 0, borrowing disabled, no extension
  commitment), empty payload. The launch `e1ca59f60b66db30aecd31855f1f6ccdbad38dd5061f1dd30a5368ebe2b973db` (payload `KFORGE6` with
  the token's name, ticker and curve parameters) creates the curve covenant `3236f22c...` and moves the whole supply into its
  custody (owner scheme 4); its token input reveals the 7,214-byte redeem script, which is this program with that state and hashes
  to the genesis output's P2SH. Both transactions rebuild to their ids and run in KOB's engine; `kob_protocol::registry::verify_genesis_with`
  verifies the genesis as a fixed supply with no companion (`program_kforgetoken7214.rs`).
* **mainnet**: the curve custody of GUCCI, FKING and PUP (1,000,000,000 base units each, owner = the token's `rungCovId`, scheme 4) is a
  live UTXO at the P2SH of this program with that state (`getUtxosByAddresses`). The GUCCI sell
  `16cc93cb8cab4d5e6cccb47d705c77f79bead56506f8149217ec01a0e8c40940` spends a holder (15,000,000 base units, owner scheme 0,
  borrowing disabled) as the leader and the curve's custody as its delegate, and returns the whole supply to the curve: both revealed
  redeem scripts are this program, KOB's builder reproduces the delegate's signature script byte for byte and the leader's push for
  push, and the token output is the P2SH of the program with the expected state.
* **Witness encoding.** KForge's builder pushes a one-byte argument group as a data push (`01 04`) where KCC-1's `PushMinimal`,
  which KOB's builder emits and KOB's strict decoder requires (`kob_protocol::kcc20::transfer_next_states`, the indexer), is `OP_4`.
  The forms differ only for a one-byte group holding 1..=16: a transfer with a single next state whose owner or borrow scheme is
  not 0, as in the launch and the sell above (the whole supply into or back to the curve's custody). KOB does not decode such a
  transfer's output (a curve or pool custody, which KOB does not track); a transfer with two or more next states, or a single
  scheme-0 holder without borrowing, encodes the same in both forms. The program accepts both.
* Not checked: the genesis of KForge's mainnet tokens (the public nodes keep block bodies for about 30 hours and KOB used no
  explorer). Each token carries `genesis_unverified` until its genesis is checked (`kob registry verify-genesis`), as for every
  program. kascov.io lists none of KForge's tokens.

## How a launch holds tokens, and what the curve and the pool can do

In the observed launch the genesis creates the whole supply (1,000,000,000 base units) in one holder of the creator's key; the launch transaction (the
curve's genesis) moves it into the curve's custody (owner scheme 4: spendable only in a transaction holding an input of the curve's
covenant id, whose script then binds the amounts). KForge sells 650,000,000 base units on the curve in lots of 1,000,000 and keeps
350,000,000 (`poolTokenReserve`) for its AMM pool, another covenant of its own, which takes over at graduation. Neither the curve nor
the pool is in the token's genesis group, and KOB did not review either. They can move only the tokens they own: a holder (owner
scheme 0) moves only with its key's signature, or, if its borrow scheme allows it, on the borrow path that returns the cell to its
owner with strictly more tokens and no less KAS (the observed mainnet holder has borrowing disabled). Whatever the `burned`
amounts of KForge's API stand for (KOB did not trace them), a transfer conserves the amount exactly, so supply never grows.
KForge's artifact also carries the app's `PublicMint` and `TokenSeed` issuers built against this token template; a KForge launch
uses neither, so the program's profile is `minters: genesis-only` with no companions (a genesis that put an issuer in the token's
group would not verify).

Decimals: KForge declares none (the genesis has no payload, the `KFORGE6` launch payload and the API name none; its site was not
reachable for reading). A launch mints 1,000,000,000 base units and KForge shows its tokens as whole units, so KOB uses 0 for a token of
the program that declares none: the profile's `default_decimals`, source "KForge convention" (maintainer decision 2026-11-05; a token
declaring decimals under KCC-23, or a list's decimals, comes first).

## In KOB

Embedded as `KForgeToken7214` (`crates/kob-protocol/data/programs.json`): `reviewed` (internal review, 2026-11-05), `networks: [mainnet,
testnet-10]`, on the shipped registry's strict list (`kforge-token-7214`), so a mainnet build trades its tokens without a registry
entry. It is a KCC-20 codec program (custody 4, holder 0, witness `kcc20-transfer`): the KCC-20 order kinds serve it unchanged, within
the program's 6 token inputs / 6 token outputs. KOB offers only its own book; a token still on KForge's curve or in its pool trades there
too, and KOB does not route to either. Tests: `crates/kob-tests/tests/program_kforgetoken7214.rs` (provenance, the observed cells,
owners, conservation, slots, carrier floor, KOB orders) and `kcc20_public_mint_tests.rs` (the order grid on its holders, six custodies
and a seventh refused, the context-field and program swaps); the review: `docs/spec/token-programs.md` section 6.

The older entry `../kforge/` (`KForgeToken1785`, an earlier KRON-layout KForge program observed on testnet-10, which KForge's current
registry no longer names) is legacy: testnet-10 only, `pending-review`, kept so that those tokens stay readable.
