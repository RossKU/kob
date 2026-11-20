# Automatic creator lock

Status: implemented (`contracts/locks/KobLockAuto.sil`, `crates/kob-protocol/src/lock_auto.rs`).
Decision: founder, 2026-11-20. New creator locks open by themselves after their date; the manual lock
(`docs/spec/token-metadata.md`, *Creator lock*) stays as an option (web Settings "Manual unlock", CLI
`--lock-manual`), and locks created the old way keep working with the manual unlock.

## What it does

A creation locks the share its creator keeps until a calendar time (`until`, unix ms, the node's past
median time). After `until` **anyone** may send the release. The release can only pay the creator: the
tokens, whole, to the creator's key, and the lock coin's KAS back to the creator less at most
`MAX_TAKE` (0.03 KAS), which pays the fee and the sender's tip. Before `until` nobody can move it, the
creator included. Executors' keepers send releases for a 0.01 KAS tip (`crates/kobd/src/keepers/lock_release.rs`).

## How it holds the tokens

The KCC-20 programs authorise a token UTXO of owner scheme `0x03` (`p2sh/v1`) when the transaction spends
an input whose script public key is `P2SH(owner)` (the owner witness is that input's index). No KCC-20
program changes.

* The locked genesis output's owner is `Blake2b(R)`, where `R` is the redeem script of the pinned template
  `KobLockAuto` with the state (`until` temporal, the creator's x-only key, `tplPrefixLen`, `tplSuffixLen`
  of the token program: the bytes before and after its 112-byte KCC-20 state). The template hash is
  `ed392654183dfc8e0d8ff49130b15ba142b27af33ecb3a854b98ada5c45158e4` (`lock_auto::TEMPLATE_HASH_HEX`).
* The genesis also pays a plain KAS output, the **lock coin** (`AUTO_COIN`, 1 KAS), to `P2SH(R)`, after the
  token outputs and a curve, before the change. Spending the coin runs `R`.

## The script (`KobLockAuto.release()`, no argument, no signature)

| # | Check | Why |
|---|-------|-----|
| 1 | `require(tx.time >= until)`: `until >= LOCK_TIME_THRESHOLD`, then `OP_CHECKLOCKTIMEVERIFY` | unspendable before the date (the node takes the transaction once its past median time passes the lock time; the input's sequence must be non-final) |
| 2 | `this.activeInputIndex == 0`, 2 or 3 inputs, input 2 (if any) has no covenant id | a fixed shape: no second token input, no stray, no second coin |
| 3 | `OpInputCovenantId(1) != 0` and `OpOutpointTxId(1) == OpOutpointTxId(0)` | the token input comes from the same transaction as the coin: only the genesis made both |
| 4 | input 1's redeem script `rs` = the last `prefix + 112 + suffix` bytes of its signature script, and `P2SH(rs)` = its script public key | the state read is the real one |
| 5 | the state's owner bytes are `0x20 ‖ H`, scheme `0x03`, borrow `0x00`, and `P2SH(H)` = input 0's script public key | the token input is this lock's |
| 6 | `OpCovOutputCount(tok) == 1`, `OpOutputCovenantId(0) == tok`, output 0's script = `P2SH(rs` with owner = creator key, scheme `0x00)`, the amount and the rest copied | the tokens go whole to the creator's key |
| 7 | `outputs[0].value >= inputs[1].value` | the token UTXO's KAS stays with it |
| 8 | output 1 = `P2PK(creator)`, `outputs[1].value + MAX_TAKE >= inputs[0].value` | the coin's KAS goes back, at most 0.03 KAS kept |

`MAX_TAKE` = 3,000,000 sompi: a tip of 0.01 KAS (`RELEASE_TIP`) and up to 0.02 KAS of fee (a release of
the default program `KCC20PublicMint` pays about 0.01 KAS at the relay floor, 100 sompi/gram). A sender who
wants to pay more fee adds it from its own input 2.

## Attacks considered

* **Early release.** CLTV with a non-final sequence (check 1). Refused in the engine for a lock time one
  millisecond early, 0 or a DAA score, and for a final sequence.
* **Redirect the tokens.** Output 0 is rebuilt from the input's own redeem script with the creator's key
  (check 6). Another key, another scheme (e.g. `0x04`) or the lock's own owner are refused.
* **Partial spend / split.** The amount is copied, and the token has exactly one output (check 6); the
  KCC-20 program conserves the sum, so nothing else can carry tokens. A split is refused.
* **Take the token UTXO's KAS.** Check 7.
* **Redirect the KAS.** Output 1 is the creator's P2PK and gets the coin less at most `MAX_TAKE` (check 8).
  The worst any sender can do is keep 0.03 KAS of the 1 KAS coin.
* **Use up the coin on something else (griefing / stranding).** The coin can be spent only together with a
  token UTXO of the same transaction owned by the lock (checks 3, 5): only the genesis's locked output.
  Neither can move without the other, so they are released together or not at all.
* **A coin sent to the lock's address later, or a token sent to the lock's owner later (a stray).** Its
  outpoint is of another transaction (check 3): it can never be used to spend the locked output, and a
  stray sent to the lock's owner is stuck (like a burn). Strays of other tokens cannot ride along (check 2).
* **Merge or borrow paths.** With 2 or 3 inputs and input 2 plain, the locked UTXO can only be its token's
  leader; the locked state has borrowing disabled (the reader checks it).
* **A wrong prefix or suffix length (a lock that can never open).** The lengths come from the token program
  the genesis is proven under; the builder derives them (`AutoLock::new`) and the reader derives the
  script from the terms and that program (`verify_genesis_lock`), so a lock with other lengths holds
  nothing and is not shown.
* **Fees.** Nobody signs, so a sender cannot be forced to pay; the coin pays. Under fee pressure the creator
  or a keeper adds an input.

## Declaration (KCC-23)

`"lock": "<88 hex>"` holds the **terms**, the 44-byte manual template of (`until`, key), and
`"lock_kind": "auto"` says how it opens (the last member KOB writes). A manual lock has no `lock_kind`
(or `"manual"`). Another `lock_kind` drops the lock (named in `issues`). A reader of an auto lock derives
`R` from the terms and the program of the proven genesis states and verifies against the genesis:

* exactly one genesis output is held by `Blake2b(R)` (scheme `0x03`, borrow disabled);
* a plain genesis output of at least `AUTO_COIN_MIN` (0.2 KAS) pays `P2SH(R)` (the coin).

`VerifiedLock` then has `kind: "auto"`, `script` = `R`, `authority` = `Blake2b(R)` and `coin` = the coin's
index and amount. An older reader sees a manual lock that holds nothing, and shows no lock.

## The release

`lock_auto::build_release` (kob-wasm `buildRelease`, `kob token unlock`, the keeper): inputs coin and
locked UTXO (and a keeper's KAS input), outputs the tokens and the creator's KAS (output 1 is the change
slot) and, with a keeper, the keeper's input back plus the tip. Lock time `until`. The builder refuses a
coin and a token UTXO of different transactions, a state the lock does not hold, a tip without a keeper
input, and a fee plus tip above `MAX_TAKE`. The coin's compute budget is measured per program
(`auto_budget`): 3 for `KCC20Ref`, 5 for `KCC20PublicMint`.

## Finding due locks

The indexer keeps the automatic locks by time (`kcc23_auto_locks`). `GET /v1/creator-locks?due=<ms>`
lists those whose `until` is at most `<ms>` and whose locked output is unspent, each verified again. The
keeper of `kobd --executor` reads the same rows in process.

## Tests

`crates/kob-tests/tests/kcc20/lock_auto_tests.rs` (every refusal above in the v2.1.0 engine, the budgets);
`crates/kob-cli/src/token.rs` `an_automatic_lock_is_released_by_anyone` (mock node: create, refused before
the time, released with no key after it); the keeper's test in `crates/kobd`.
