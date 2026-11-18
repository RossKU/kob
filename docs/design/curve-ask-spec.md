# KobCurveAsk: a stepped sell curve in one order

Status: built on branch `curve-ask` (2026-11-17), rides the bid-delivery re-version. Prototype and its measurements:
branch `curve-proto` (`contracts/prototypes/curve/`, `tests/design/curve_ask_experiments.rs`).

One sell order, one custody UTXO, whose price rises level by level along a price table (Hyperliquid HIP-2, sell side
only). Contract: `contracts/orders/KobCurveAsk.sil`; engine tests: `crates/kob-tests/tests/kob/kob_curve_ask_tests.rs`;
ablation catalog: `tools/ablation/catalogs/curve-ask.mjs`.

## 1. State

Encoding as every template: `int` = `0x08` + 8 bytes LE, `byte[32]` / `pubkey` = `0x20` + 32 bytes. Offsets are state
coordinates (redeem = state + 1).

| Field | State offset | Notes |
|---|---|---|
| `maker` | 0 | payouts and the cancel key |
| `tokenCovId` | 33 | 0 = unbound (§4); redeem window `[35, 67)`, written once by the first fill |
| `tokenTplHash`, `tplPrefixLen`, `tplSuffixLen` | 66, 99, 108 | the custody's token program, as `KobAsk` |
| `scale` | 117 | base units per whole token |
| `minFill` | 126 | as `KobAsk` |
| `tip` | 135 | sompi per whole token, taken from every level |
| `activeFrom` | 144 | no fill before this DAA score |
| `levelSize` | 153 | base units per level, > 0 |
| `total` | 162 | base units the curve sells in all; immutable |
| `amountLeft` | 171 | the only mutable field: redeem window `[173, 181)`; the custody holds exactly this |
| `extensionCommitment` | 180 | of the custody |
| `table` | 213 | `int[]`, `nLevels` prices in sompi per whole token, one minimal push of `8·nLevels` bytes (direct, `OP_PUSHDATA1` or `OP_PUSHDATA2`) |

State length `213 + header + 8·nLevels` (header 1 to 3 bytes). Every field before `table` has a fixed size, so the
windows do not depend on the table length, and one template hash serves every table (`rsLen`, §3).

Level j sells the units `[j·levelSize, (j + 1)·levelSize)` of `total` at `table[j]`; the last level may be partial.
The current level is derived, `(total − amountLeft) / levelSize`, never stored.

## 2. Entries

- `settle(byte[8] nb, int tokenIn, int tokOut, int rsLen)`: a fill of `n = int(nb) > 0`. Anyone, from `activeFrom`.
  `tokenIn` is the custody input, `tokOut` the rest custody output, `rsLen` the length of this order's redeem script.
- `cancel(sig)`: maker only, SIGHASH_ALL. Also the amend (§5).

There is no refund and no expiry. A curve rests until it sells out or the maker cancels it. An on-chain expiry costs
every fill its refund branch, the shared token pin it needs and two state fields: 148 B (0.0003 KAS per fill), so it is not free; the free expiry is the
ORDER record's `deadline` (soft, matcher-enforced; the maker cancels). No IOC/FOK, TWAP or decay.

## 3. Fill rules

1. Custody as `KobAsk`: the token input `tokenIn` has the token's covenant id, is an instance of `tokenTplHash`, is
   owned by the order's id (scheme `0x04`, borrowing disabled), carries `extensionCommitment` and exactly `amountLeft`.
   Stray guard as `KobAsk` (`MAX_TOK_IN` 8).
2. `n > 0`, `n ≤ amountLeft`, `n ≥ minFill` unless `n = amountLeft`; `scale > 0`, `tip ≥ 0`, `levelSize > 0`,
   `amountLeft ≤ total`.
3. Walk, unrolled `MAX_CROSS = 8` times: from `pos = total − amountLeft` to `end = pos + n`, each step takes the rest of
   the current level or of `n`, at `p = table[lvl]`, requires `p ≥ tip` and adds `quoteOf(take, p − tip, scale − 1)`.
   Then `pos == end`: a fill touches at most 8 levels, counting the partly sold one it starts in. A level past the table
   fails the script on the read (fail closed).
4. Proceeds `allIn` are paid to the maker's P2PK at output `i` (the order's input index). Rounding: up, per level.
5. A partial fill continues the script with `amountLeft − n` (and, if unbound, `tokenCovId` written), exactly one output
   of the order's id, at least the order's KAS; the rest custody at `tokOut` is pinned as `KobAsk`'s, with at least the
   custody's KAS. A sell-out has no output of the order's id and pays `allIn` plus both carriers at output `i`.
6. `rsLen`: the redeem script is the last `rsLen` bytes of the signature script and its P2SH must be the input's, so a
   wrong `rsLen` fails.

## 4. Created in the token's genesis (unbound)

A KIP-20 covenant id commits to the scripts of its genesis outputs. A custody that names the order as owner and an order
that names the token cannot both be created in one transaction, unless the order leaves `tokenCovId` zero:

- Unbound (`tokenCovId == 0`): the first settle takes the token of the custody input, which must come from the same
  transaction as the order UTXO (`outpointTxId` equal). While unbound the order UTXO is still its genesis output, so that
  transaction is the maker's. A partial fill writes the id into the continuation; a bound order never reads the rule.
- Builders create unbound curves only in a token genesis whose every output is the maker's (`issue::build_creation_with_curve`).
  A third party who could add outputs to that transaction could plant a second custody of another token.
- The genesis payload is a KOB1 payload: the curve's ORDER record and the token states as an optional `GENESIS` record
  (`0x85`, the `KOBG` bytes, `genesis_states`).
- The genesis spends the KCC-23 preparation's output 0 alone, which authorises both groups, so KCC-23 also binds the
  declaration to the curve's covenant id; a KCC-23 reader lists only covenants whose outputs are a token program's.

## 5. Placement, gates and records

- Kind code `0x0b`, family `0x01` (KCC-20 only; no KRON twin in this package). One custody part (`extensionCommitment`).
- Compact state: as every template, with `table` spelled `count:LEB128` then one LEB128 per entry.
- Gates (`check_new_order`, builders and indexers): `scale` a power of ten `≤ 10^9`; `1 ≤ nLevels ≤ 1,000`;
  `levelSize > 0`; `0 < amountLeft ≤ total ≤ nLevels·levelSize`; `tip ≥ 0`; every price `≥ tip` and `> 0`;
  `levelSize·max(table)/scale < 2^62` (128-bit); `minFill ≥ 1`; `activeFrom ≥ 0`; `tokenCovId` may be zero.
  A crossing fill that sums several levels near the bound overflows and fails closed; one level always fits.
- Indexer rule 5 for an unbound order: the custody part's output must be in the same transaction, a genesis of a
  supported program; `tokenCovId` is that output's covenant id. An unbound curve outside a token genesis is not listed.
- AMEND (kind `0x0b`): keeps `maker`, the token fields, `extensionCommitment`, `scale` and `amountLeft`; may change
  `minFill`, `tip`, `activeFrom`, `levelSize`, `total` and `table` (the gates again, `total ≥ amountLeft`). An amend of an
  unbound order must bind it: the new `tokenCovId` is the custody's token.
- Several curves per token are allowed; nothing on chain or in the gates prevents it. One curve is one UTXO: one fill
  per transaction, chained takers serialize (accepted).

## 6. Fees and budgets

Measured with 1 KAS carriers on the reference 3 / 3 program (`kob_curve_ask_tests::curve_sizes_and_fees`); the fee is
the relay floor.

| Levels | Redeem B | State B | Partial fill: tx B / fee KAS | Sell-out: tx B / fee KAS | Cancel: tx B / fee KAS |
|---|---|---|---|---|---|
| 10 | 2,478 | 295 | 6,330 / 0.01266 | 6,050 / 0.01210 | 6,050 / 0.01210 |
| 50 | 2,799 | 616 | 6,651 / 0.01330 | 6,371 / 0.01274 | 6,371 / 0.01274 |
| 100 | 3,199 | 1,016 | 7,051 / 0.01410 | 6,771 / 0.01354 | 6,771 / 0.01354 |

The unbound path costs 45 B of script (+0.00009 KAS per fill) against a curve that could not be created in its genesis.
A fill's size and fee do not depend on how many levels it crosses; every table level adds 8 B to every fill. Compute
budget roles: `KobCurveAsk.settle.rest@<program>`, `….settle.close@<program>`, `KobCurveAsk.cancel@<program>`, measured
at the worst shape (1,000 levels, 8 crossed) by `kob-protocol/tests/curve_budgets.rs`.

## 7. Defaults (`defaults::curve_table`)

UI defaults: sell the whole supply, 100 levels, 10,000 KAS if sold out, last level 5× the first, geometric steps, tip
0, `minFill` worth 10 KAS at the first level (the ask default: every fill is a payout UTXO of the maker). Every number
can be set.

- `levelSize = ⌈amount / levels⌉`, `nLevels = ⌈amount / levelSize⌉` (fewer when `amount` is small); level sizes
  `s_i = levelSize`, the last `amount − (nLevels − 1)·levelSize`.
- `r = multiple^(1/(nLevels − 1))`, `p_i = p0·r^i`, `p0` solved so that `Σ s_i·p_i / scale = total`.
- Rounding: prices are whole sompi per whole token. The realised total `R = Σ ⌈s_i·p_i / scale⌉` (a sell-out bought
  level by level) is at most the target. `p0` is the largest (fixed point, `r` and `r^i` in integer Q40 arithmetic, so
  every platform builds the same table) with `R ≤ total`; then, from the top level down, each price is raised by one
  sompi while `R` stays `≤ total` and the table stays non-decreasing.
- **Chromatic shape** (`shape: chromatic`, web "Shape: Chromatic", CLI `--shape chromatic --octaves N`), after Kaspa's
  chromatic emission (its reward falls one semitone, `2^(−1/12)`, a month): one semitone up per level. `N` octaves (1 to 8)
  give `12·N + 1` levels, the start level included; `p_i = p_(i mod 12)·2^(i div 12)` with `p_k = ⌊p0·S_k⌋` and
  `S_k = round(2^(k/12)·2^40)` a fixed table (`defaults::SEMITONES`), so every 12 levels exactly double and the top is
  exactly `2^N` times the start. `p0` is solved as above; no leftover is spread over the top levels (the octaves stay
  exact). `levels` and `multiple` are not used. The contract is unchanged: the table is explicit state.
