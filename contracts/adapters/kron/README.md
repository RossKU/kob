# KRON family adapter

KOB order contracts for the KRON token family: the legacy 46-byte token state that is live on mainnet (two pinned
token programs, 2,433 B and 2,732 B, see `templates/README.md`). One order contract per order kind, a port of the
KCC-20 v2 contracts in `contracts/v2/` with only the token codec and the owner types changed. The protocol layer
stays family-blind: a family is (state codec, owner-type mapping, sigscript / witness builder, template allowlist).
A KAS-quoted order pins one `tokenCovId`, one template hash and one state codec. The orders that span two tokens are
the pair orders of a pair A/B, `contracts/v2/KobPair.sil`, `KobCondPair.sil` and `KobIfdPair.sil`: one template each
for both families, whose state names the family, program hash and lengths of each token (the custody codec of the token
it holds, the delivery codec of the token it buys), so A and B may be of different families. They have no KRON twins.

Each `v2/Kob*Kron.sil` header lists the differences from its KCC-20 twin; the same logic otherwise applies, including
the protocol v3 amount rule: amounts in token base units, prices in sompi per whole token (`scale` = 10^decimals base
units), every quote value rounded in the maker's favour (`quoteOf`, `docs/spec/order-types.md`), a minimum fill
`minFill` in every kind.

| Source | KCC-20 counterpart | Notes |
|---|---|---|
| `v2/KobAskKron.sil` | `KobAsk` | limit sell, all-in price + tip, `minFill`, decay auction (`decayStep`), TWAP (`maxFill`), IOC/FOK (killable at +600 DAA), soft expiry + 90-day idle bound, exact custody (`amountLeft`) and stray guard |
| `v2/KobBidKron.sil` | `KobBid` | limit buy, no `extensionCommitment` state field (KRON has none), `minFill`, `decayStep`, IOC/FOK kill, refuses token inputs in refund, stray guard in fill |
| `v2/KobCondAskKron.sil` | `KobCondAsk` | stop, stop-limit (auction over `bandDaa`), trailing stop (multi-step), take-profit, OCO; `keeperTip`; exact custody; triggers from a `KobAskKron` / `KobBidKron` fill in the same transaction (touch); exit of repeat IFD (`parent`, `rptPrice`, `rptUntil`) |
| `v2/KobCondBidKron.sil` | `KobCondBid` | buy-stop, buy-stop-limit, trailing buy, limit buy, OCO; same lifecycle fields; exit of repeat sell-first IFD |
| `v2/KobIfdBidKron.sil` | `KobIfdBid` | if-done buy (IFD/IFO/bracket), partial fills, stop entry, `minFill`, repeat (`rptAmount`, merge), exit = `KobCondAskKron` |
| `v2/KobIfdAskKron.sil` | `KobIfdAsk` | sell-first if-done, partial fills, stop entry, repeat (merge into the entry custody), `close`, exit = `KobCondBidKron` |
| `AskOrderKron.sil`, `BidOrderKron.sil` | `AskOrder`, `BidOrder` | v1 plain orders (used by the v1 test suite) |

## Token codec and owner types

State (template offset 0, 46 B): `0x20 owner[32] | 0x01 id_type | 0x08 amount (LE i64) | 0x01 is_minter`.
`id_type`: 0 pubkey (token-level signature), 1 script hash, 2 covenant id, 3 address presence.

- **Custody**: the order holds one token UTXO with `id_type 2`, `owner = the order's covenant id`,
  `is_minter 0` (all three are required on the input and pinned on every continuation output). The token
  program authorises such an input only when an input with that covenant id, i.e. the order script, is in the
  same transaction.
- **Maker deliveries** (ask refund, IOC return, bought tokens of a bid or conditional buy):
  **`id_type 3` (address presence) by default**, the type KRON's SDK and wallets use for user balances, so a
  KRON wallet shows and can spend them without any KOB tooling. Type 3 means any transaction that includes a
  P2PK input of the maker can move those tokens (the wallet's SIGHASH_ALL signature covers all outputs). The
  alternative is `id_type 0` (needs a token-level signature; less wallet-friendly). It is one constant per
  contract, `TYPE_DELIVERY = 0x03` (`KRON_DELIVERY` in the pair templates); switching means changing that constant,
  rebuilding the artifacts (`scripts/build-contracts.sh`) and re-pinning the template hashes.
- **Never accepted**: `is_minter = 1`, foreign owners, `id_type 0/1` on custody or continuation outputs.

## KRON token limits that shape the orders

- 1..4 token inputs and 1..5 token outputs per transaction (the KCC-20 reference build is 3/3, KOB-issued
  KCC-20 is 8/8), every token input runs the whole check itself (no leader): a batch of k asks carries the
  token program k times.
- Every output amount must be `1 <= amount <= 1e9`. Order quantities above `1e9` base units can never be
  moved; the KRON wallets and the launchpad never create such UTXOs, and a bid whose delivery would exceed `1e9`
  is simply unfillable for that size (the maker's own limit). The builders refuse a pair order whose KRON amount or
  custody exceeds `1e9`.
- A token input names its authorising input in a one-byte witness that the program reads as a signed number, so the
  authorising covenant input of a KRON token input must sit below index 128 (the builders place it so).
- The KRON token program does not embed its covenant id, so identity is `(covenant id, template hash)`;
  KRON has shipped two templates in two months, so each new one needs a semantic review (one entry, no admin
  path, `id_type 2` semantics, conservation) before it is allowlisted in `registry/tokens.json`.

## Order rules on KRON

The lifecycle rules of `contracts/v2/` apply with the KRON codec and limits:

- **Exact custody.** Asks, conditional sells (incl. every if-done exit) and if-done sells of a KRON token carry a
  mutable `amountLeft`; `tokenIn` must be the KRON UTXO with owner = the order's covenant id (`id_type 2`, state
  `[1..33)`) and amount (state `[36..44)`) exactly `amountLeft` base units, so dust or strays sent to the order's id
  can never stand in for the custody. A pair order holding a KRON token (its S custody, a pair bid's escrow, a sell-first
  pair entry's A custody or B prefund) requires the same of that custody with the exact amount its state names.
- **Stray scan.** `noStrays` walks the transaction's inputs that carry the token covenant id (at most
  `MAX_TOK_IN = 4`, KRON's limit; more are refused) and requires that none other than the custody has the order
  id as owner; the owner is read at `sigscript_len - tplSuffixLen - 46 + 1` (the redeem script is the last push).
  Bid-side adapters own no tokens: fill runs the scan, refund refuses any token input and every `update` any token
  input owned by the order id (the evidence fill next to it brings token inputs of its own).
  Only the maker's `cancel` (SIGHASH_ALL) moves strays.
- **Auctions, IOC/FOK kill, stop auctions and `armed`, `keeperTip`, multi-step trailing, stop entries, `minFill`
  and `minTouch` (base units)**: same logic as `contracts/v2/`, only the codec and the limits differ.
- **Repeat IFD / IFO.** `rptAmount` on `KobIfdBidKron` / `KobIfdAskKron`; booked exits carry `parent`,
  `rptPrice` (`rptPre`) and `rptUntil`; a booked take-profit merges its entry (`settle` / `fill` with
  `-(k*2^53+m)`, both sides checking the other's leading 8-byte push) so the entry's covenant id persists across cycles;
  an exit's `update` never runs next to its entry. Sell-first repeat re-creates the entry custody from the bought-back
  amount (`id_type 2`, owner = the entry) and `close()` refunds an empty repeating entry.
  Day orders need no on-chain rule (the wallet sets `expiryDaa` for 00:00 UTC).
- **Touch trigger.** Stop legs, trailing ratchets and stop entries read their trigger evidence from a plain
  `KobAskKron` / `KobBidKron` filled in the same transaction (`ev`, and `tk` for an ask's custody: the KRON input
  with `id_type 2` and owner = the ask's id), exactly as the KCC-20 kinds (`docs/spec/matcher.md` section 4).
  There is no receipt covenant and no genesis.

Sizes (bytecode B / state B): KobAskKron 1,487 / 243, KobBidKron 1,343 / 252, KobCondAskKron 3,178 / 330,
KobCondBidKron 2,928 / 348, KobIfdBidKron 3,498 / 552, KobIfdAskKron 4,397 / 561 (the pair orders for KRON tokens: `KobPair` 2,960 / 447,
`KobCondPair` 6,284 / 543, `KobIfdPair` 8,834 / 909, one template each for both families).

## Layout notes (hand-coded byte windows)

Same technique as the KCC-20 contracts. Compared to them, the KRON bid-side states carry no
`extensionCommitment` (33 bytes less), so the splice windows are (redeem-script offsets; the state span
starts at 1): `KobCondAskKron` stopPrice `[182..190)`, armed `[245..253)`, amountLeft `[272..280)`;
`KobCondBidKron` stopPrice `[191..199)`, amountLeft `[254..262)`, armed `[263..271)` (state 348 B);
`KobIfdBidKron` amountLeft `[128..136)`, armed `[254..262)`, rptAmount `[263..271)` (state 552 B);
`KobIfdAskKron` armed `[245..253)`, amountLeft `[254..262)`, rptAmount `[263..271)` (state 561 B; its own state has no
token-codec field, so these offsets equal `KobIfdAsk`'s). It commits a 288-byte `KobCondBidKron` exit state (348 B with
the repeat fields) with amountLeft at state payload `[253..261)` and the parent at `[289..321)` of a booked exit. The
evidence reads of the conditional kinds take the bid fields 33 bytes earlier than the KCC-20 ones (bid state 252 B,
ask state 243 B); the KCC-20 `KobAsk` (276 B) and `KobCondAsk` (363 B) have the same fields plus `extensionCommitment`
as their last one, so the offsets above are theirs too. The harness asserts
every window against the compiled state.

## Constructor placeholders

`*.ctor.json` hold size-measurement placeholders except for the template chain, which is real (inlined build
constants, the same on every network): `KobCondAskKron` and `KobCondBidKron` embed the evidence templates
`KobAskKron` and `KobBidKron` (hash, prefix and suffix length), `KobIfdBidKron` the `KobCondAskKron` exit and the
`KobBidKron` evidence template, `KobIfdAskKron` the `KobCondBidKron` exit and the `KobAskKron` evidence template.
Token arguments are the pinned KRON 2,433 B template (`prefix 0`, `suffix 2387`). Real orders are fixed by the
tests and the protocol library (per-order values are state).

## Protocol library (`kob-protocol`)

The builders of `crates/kob-protocol` cover the KRON family like KCC-20: every user action (creation
with the token move into id_type 2 custody, cancel, cancel-replace, position cancel, refunds, send), every
matcher shape (N:M batches within 4 token inputs / 5 outputs, conditionals armed by a fill of the batch, IFD / IFO with repeat
merges, auctions, strays) and the keepers. The family is the token program's (`KronToken2433` /
`KronToken2732`), and the order kinds are `KobAskKron` .. `KobIfdAskKron`. Differences the builders handle:
token inputs have no leader: every one carries the next-state columns of the whole covenant group and one
witness byte naming its authorising input (the order input for id_type 2, a P2PK input of the owner for
id_type 3, so a transaction that spends key-held KRON tokens must include a funding input of that key); token
outputs are bound to the first token input; outputs must hold 1..=1e9 units. `crates/kob-protocol/tests/`
validates every builder output on both real templates in the v2.1.0 engine (`kron.*` golden vectors).

Tests: `crates/kob-tests/tests/kob_kron_v2_tests.rs` (sell side) and `kob_kron_v2_buy_tests.rs` (buy side),
both against the real KRON bytes of both templates; `kob_kron_adapter_tests.rs` is the v1 suite.
Nothing here has been independently audited yet (pre-audit).
