# KOB1 transaction payload and the placement record

Status: normative. Payload version `0x04` carries the compact placement record and the in-place amend record of the
protocol v3 templates (amounts in base units, prices per whole token: `order-types.md`, *Amounts, prices and rounding*).
Version `0x02` is what a payload without order records is written as; an `ORDER` record of version `0x02` and every
payload of version `0x03` described templates this build does not pin and are rejected, like any other version.
Record type `0x02` and kind `0x07` are reserved, never reused.
Implemented by `kob_protocol::payload` (`crates/kob-protocol/src/payload.rs`), exposed in wasm as `encodePayload` /
`decodePayload` / `recoverOrders` / `recoverAmends`. Test vectors: `payloads` and every `create.*`, `cancelReplace.*`,
`amend.*`, `route.*` and `pair.*` entry in `crates/kob-protocol/vectors/golden.json`, the same shapes for KRON tokens
under the `kron.` prefix (`kron2732.` for the second KRON program; pair orders by the families of A and B:
`pair.kcc20-kron.`, `pair.kron-kcc20.`, `pair.kron-kron.`).

This document is the byte-level definition of the **placement record** required by
`matcher.md` §1.1: the placement record *is* the `ORDER` record below. `matcher.md` states what a
wallet must publish and what indexers and matchers do with it; this file states how it is encoded
and validated.

## Purpose

A Kaspa transaction carries at most one payload. KOB uses it for four things:

1. **Order visibility (placement record).** Order UTXOs are P2SH, so an order's parameters are not
   on chain until it is spent. A transaction that creates orders (a create, the replacement of a
   cancel-replace) says which outputs are KOB covenants, of which template, with which state, where
   their token custody is and, for a day order, until when conforming matchers fill it. Indexers
   list an order only from its placement record (`matcher.md` §1.1: an order without a valid record
   is invisible); a maker who lost the indexer can recover an order from the genesis transaction
   alone (CLI `recover`). Continuations, if-done exits and repeat merges need no new record: they
   are derived from the spending transactions (`matcher.md` §1.1, §6, §6.1).
2. **In-place amends (AMEND record).** A plain ask's or plain bid's `cancel` (maker, SIGHASH_ALL) constrains no
   output, so the maker can continue the order's covenant id with new terms; an ask's custody, owned by that id, stays
   where it is and no token program is revealed (a bid owns no tokens: its quantity is its escrow). The new state is not
   derivable from the spend: the AMEND record announces it.
3. **Coexistence with x402.** A swap-and-pay or x402 payment that also creates or cancel-replaces
   an order, or pays through a two-token route, carries its payment reference in the same payload
   as an `X402` record, so one transaction can do both.
4. **Client tags** (optional, informational).

**The payload is never trusted.** Everything in it is re-derived from the transaction itself
before use (see *Validation*). A wrong or missing payload cannot create, alter or hide an order's
funds; it only makes the order invisible to conforming matchers.

## Encoding

```
payload  = magic version record*
magic    = "KOB1"                      4 bytes: 4b 4f 42 31
version  = 0x04 | 0x03 | 0x02          1 byte
record   = type:u8 length:u16le value[length]
```

* Records follow each other until the end of the payload; there is no count field.
* Fixed-width integers are little-endian. `LEB128` is unsigned LEB128 (7 bits per byte, low group first, the high bit
  set on every byte but the last) in its shortest spelling: a decoder rejects a trailing zero group (`80 00`), more
  than 10 bytes and bits beyond 64, so every value has exactly one encoding.
* **Version.** An encoder writes version `0x04` when the payload carries an `ORDER` or `AMEND` record and version
  `0x02` otherwise (an x402 commitment or a note is the same bytes in every version, and stays what
  `x402-kcc20-profile.md` specifies). A decoder accepts both; `ORDER` and `AMEND` records exist in version 4 only
  (*ORDER record* below).
* **Critical and optional types.** Types `0x00..=0x7f` are critical: a decoder that does not know a
  critical type rejects the whole payload. Types `0x80..=0xff` are optional: unknown ones are
  skipped (and preserved by re-encoders).
* A payload with another magic is not KOB1; decoders return "not KOB1", not an error.
* A decoder rejects: another version, a truncated header or record, a length that runs past the
  end, an unknown or reserved critical type (`0x02`; `0x01` and `0x03` under version 2), an ORDER record of a reserved
  kind (`0x07`), an AMEND record of a kind that is not amended in place, a record with bytes left over or a non-canonical
  spelling (below), or a record whose value does not have the size given below (X402 1 to 64 bytes, NOTE at most 64).
  Encoders refuse the same, and an ORDER or AMEND record whose state length is not the template's or whose state does
  not decode canonically (validation rules 1 and 2). The content of an optional record never fails the payload: a NOTE
  whose bytes are not UTF-8 decodes as an unknown optional record (type `0x82`, kept verbatim).

### Record types

| Type | Name | Critical | Value |
|---|---|---|---|
| `0x01` | ORDER | yes | the placement record, below |
| `0x02` | reserved | yes | never reused: a decoder refuses a payload that carries it |
| `0x03` | AMEND | yes | an in-place amend (version 4), below |
| `0x81` | X402 | no | x402 payment reference, 1 to 64 opaque bytes (the facilitator's payment id / nonce) |
| `0x82` | NOTE | no | UTF-8 client tag, at most 64 bytes (for example `kob-web/0.1`) |
| `0x83` | SWEEP | no | a maker's sweep in place (any version): `output:LEB128 input:LEB128`, below |

### Compact state

The state span of every order template is a fixed sequence of canonical pushes (`0x20` + 32 bytes for a key, a
covenant id or a hash, `0x08` + 8 bytes for an integer, `OP_PUSHDATA1/2` + the bytes of a committed if-done exit
state), the same for every instance of the template (`kob_protocol::payload::state_layout`, read from the pinned
artifact). A record spells the state without the push headers:

```
state     = zeroMask:LEB128 field*          every field of the template, in order
field     = LEB128                          an 8-byte field (an integer): its 8 bytes read as a little-endian u64
          | 32 bytes                        a 32-byte field, unless bit k of zeroMask is set (k = its rank among the
                                            template's 32-byte fields): then it is all zero and not spelled
          | n bytes                         any other field (the committed exit state of an if-done entry), raw
```

The decoder rebuilds the exact state span (push headers from the template, zero fields from the mask). It rejects a
mask bit beyond the template's 32-byte fields and an all-zero 32-byte field spelled out instead of masked.

### ORDER record (placement record), version 4

```
output        LEB128    index of the order's genesis output in this transaction (at most 65535)
family        u8        token family: 0x01 = KCC-20 (reference layout); 0x02 = KRON-46 (46-byte KRON token state)
kind          u8        template kind code (table below); the pinned template of that kind in that family
flags         u8        bit 0: a deadline follows; bit 1: the first custody's extension commitment is all zero
                        (KCC-20 custody only); bit 2: the second custody's (a sell-first KobIfdPair's B prefund)
                        extension commitment is all zero (KCC-20 only); every other bit must be 0
state                   the order's state span, compact (above)
custody*                one part per custody the state holds, in order (below); none for a KAS-holding order:
  tokenOutput   LEB128    index of the token output holding that custody
  extCommit     32        KCC-20 extension commitment of that token output, unless its flag bit (1 for the first
                          part, 2 for the second) is set (then zero, not spelled; a spelled all-zero commitment is
                          rejected). A KRON custody has none: no bytes, its flag bit clear
deadline      LEB128    when flag bit 0: wall-clock deadline of a day order, UTC unix seconds; conforming matchers
                        do not fill at or after it
```

The custody parts follow from the decoded state (`kob_protocol::payload::custody_families`), each spelled in the family of
its own token:

| Kind | Custody parts |
|---|---|
| `KobAsk`, `KobCondAsk`, `KobIfdAsk` (both families) | one: the custody of the order's token (exactly `amountLeft`) |
| `KobBid`, `KobCondBid`, `KobIfdBid` | none |
| `KobPair`, `KobCondPair` | one: the custody of S, the token the order sells (A for an ask, B for a bid; exactly `custody`) |
| `KobIfdPair` buy-first (side 2) | one: the B escrow (exactly `custody`) |
| `KobIfdPair` sell-first (side 1) | the A custody (exactly `amountLeft`), then the B **prefund** custody (exactly `custody`) when `custody > 0` |

A record with a missing or an extra custody part, or a flag bit for a part it does not have, is rejected. An order's
KCC-20 custody part must spell the extension commitment its state pins for that custody (`extensionCommitment` of
`KobAsk` / `KobCondAsk`, the committed exit's `extensionCommitment` of `KobIfdAsk`, `sExt` of `KobPair` / `KobCondPair`,
`aExt` / `bExt` of `KobIfdPair`); any other is rejected.

The record carries no template hash: the family and the kind name the pinned template, so a payload cannot introduce a
new template (new templates need a new protocol version). The custody amount is not recorded: it is `amountLeft` of the
recorded state (exact custody, base units), and the record is only valid if the custody output holds exactly that.

The kind codes are the same in both families; the family byte selects the template set. The KRON
templates (`KobAskKron`, ..., `KobIfdAskKron`) have the codes and state lengths below. The pair kinds are one template
each for both families: kinds `0x08`, `0x09` and `0x0a` name `KobPair`, `KobCondPair` and `KobIfdPair` in either family,
and the record's family is the family of the pair's base token A.

| Code | Template | State bytes | Custody part | Pinned template hash |
|---|---|---|---|---|
| `0x01` | `KobAsk` | 276 | yes (`extensionCommitment`) | `070bb3b2…63b887c` |
| `0x02` | `KobBid` | 285 | no | `b995661f…5e54fa3` |
| `0x03` | `KobCondAsk` | 363 | yes (`extensionCommitment`) | `d9ed37c5…31e6adb` |
| `0x04` | `KobCondBid` | 381 | no | `4300602a…1334d36` |
| `0x05` | `KobIfdBid` | 585 | no | `1ba2519b…a51240d` |
| `0x06` | `KobIfdAsk` | 594 | yes (its exit's `extensionCommitment`) | `edfb30d9…747cffd` |
| `0x07` | reserved | — | — | never reused: a record of this kind is refused |
| `0x08` | `KobPair` (pair order, token A KCC-20) | 447 | the custody of S (`sExt`) | `c95c9234…ab4a9c6` |
| `0x09` | `KobCondPair` (pair conditional / exit, token A KCC-20) | 543 | the custody of S (`sExt`) | `7b8f1a9e…9a6f06e` |
| `0x0a` | `KobIfdPair` (pair if-done entry, token A KCC-20) | 909 | buy-first: the B escrow; sell-first: A, then the B prefund | `466ed2ef…827d9b6` |

KRON (family `0x02`), same kind codes:

| Code | Template | State bytes | Custody part | Pinned template hash |
|---|---|---|---|---|
| `0x01` | `KobAskKron` | 243 | yes (token output only) | `f7274b79…ef8f76d` |
| `0x02` | `KobBidKron` | 252 | no | `6ec1a3dd…d888efc` |
| `0x03` | `KobCondAskKron` | 330 | yes (token output only) | `6c4f92ce…fd35839` |
| `0x04` | `KobCondBidKron` | 348 | no | `fb392f88…a7ed953` |
| `0x05` | `KobIfdBidKron` | 552 | no | `e5cff7ed…053496d` |
| `0x06` | `KobIfdAskKron` | 561 | yes (token output only) | `d523d794…9f014f6` |
| `0x07` | reserved | — | — | refused |
| `0x08` | `KobPair` (token A KRON; the same template as in family `0x01`) | 447 | the custody of S (`sExt`) | `c95c9234…ab4a9c6` |
| `0x09` | `KobCondPair` (token A KRON; the same template) | 543 | the custody of S (`sExt`) | `7b8f1a9e…9a6f06e` |
| `0x0a` | `KobIfdPair` (token A KRON; the same template) | 909 | buy-first: the B escrow; sell-first: A, then the B prefund | `466ed2ef…827d9b6` |

Full hashes: see `kob_protocol::artifacts::PINNED` (the artifacts in `contracts/artifacts`, also the `templates` list of
the golden vectors).
An ORDER record is critical, so a decoder that does not know its kind rejects the payload (and lists nothing of that
transaction) instead of misreading it.

The family byte of a pair order record is the family of its base token A and must equal the family the state names
for A (`KobPair` / `KobCondPair`: `sFamily` of an ask, `tFamily` of a bid; `KobIfdPair`: `aFamily`); its other token is
named by the state (covenant id, program hash, prefix and suffix lengths, family 1 = KCC-20 or 2 = KRON, scale, and the
extension commitment of the outputs the order creates of it) and may be of either family. Kind `0x08` of version 4
never names a cross limit: the v3 `KobCross` was replaced by the pair kinds before any deployment.

One transaction may announce several orders (one record each, any output order). The builders
place a created order at output 0 and its custody tokens right after it; a cancel-replace places
the replacement at output 0 as well. Day orders: `kob_protocol::defaults::day_order` (wasm
`dayOrder`) returns `expiryDaa` and the `deadline` for the next 00:00 UTC (`matcher.md` §10.10).

### AMEND record (in-place amend), version 4

```
output        LEB128    index of the continuation output (the amended order)
input         LEB128    index of the order input the maker's cancel spends
family        u8        token family, as ORDER
kind          u8        0x01 (KobAsk / KobAskKron) or 0x02 (KobBid / KobBidKron): only plain asks and bids are amended in place
flags         u8        bit 0: a deadline follows; every other bit must be 0
state                   the amended state span, compact (above)
deadline      LEB128    when flag bit 0: day-order deadline, as ORDER
```

There is no custody part: the custody does not move. An amended ask keeps the maker, the token (`tokenCovId`,
`tokenTplHash`, `tplPrefixLen`, `tplSuffixLen`), `scale` and `amountLeft`, so the custody the order already holds stays
exactly `amountLeft`; it may change `minFill`, `price`, `tip`, `tif`, `activeFrom`, `expiryDaa`, `refundTip`, `interval`,
`maxFill`, `slope`, `priceEnd` and `decayStep` (`kob_protocol::payload::check_amend_terms`). A quantity change
re-custodies: that is a cancel-replace. An amended bid keeps the maker, the token (the four fields above and the
`extensionCommitment` its deliveries carry) and `scale`; every other term may change, and its quantity follows its
escrow: the builder refuses a continuation below what funds one minimum fill at the new terms (with its
`deliveryCarrier` and `reserve`); a larger escrow (a top-up) comes from maker funding in the same transaction.
Strays owned by the id stay strays (only a cancel or a sweep, below, moves them): wallets amend in place only an order without
strays.

## Validation (indexers, `recoverOrders`)

For every ORDER record, reject the record unless all hold. In the reference code a failure of rule 1 (unknown family
or kind, a wrong state length) is a decode error that rejects the whole payload (`kob_protocol::payload::decode`, as the
builders do); `recoverOrders` is all-or-nothing per payload; the executor's indexer applies rules 2 to 8 record by record
(it re-encodes and recovers each record on its own). An order output that is not the P2SH of a pinned template (rule 3)
is an order of a template this build does not pin: it is never recovered, listed or offered to a matcher or keeper.

1. `kind` is a kind of the record's `family`. An unknown family or kind rejects the record.
2. `state` decodes as the template's runtime state and re-encodes to the same bytes (canonical
   fixed-width pushes only).
3. `outputs[output].scriptPublicKey == P2SH(prefix ‖ state ‖ suffix)` of that template.
4. `outputs[output]` has a covenant binding, the authorising input does not already carry that
   covenant id, and the id equals the consensus genesis id
   `covenant_id(outpoint(authorising input), [(i, value_i, spk_i) for every output bound to the same (input, id)])`,
   and that group is the order output alone: no other output of the transaction is bound to the id (a sibling
   would carry the order's id forever and could unlock its custody, whose owner check is only "an input carries
   this id"). Every KOB builder creates order geneses as one-output groups (placements,
   cancel-replace, and the exits of if-done / repeat fills, whose one-output genesis the parent covenant itself
   recomputes).
   So the order is a fresh covenant and nothing else can ever carry its id.
5. Token-holding kinds: the custody part is present, `amountLeft > 0`,
   the token program is looked up by the order's `tokenTplHash` (a supported program of the record's
   family), and
   `outputs[tokenOutput].scriptPublicKey == P2SH(tokenPrefix ‖ KCC20State{amount = amountLeft, owner = order id, owner_scheme 0x04, borrow_scheme 0x00, borrow_guard 0, extCommit} ‖ tokenSuffix)`
   (KRON: `KronState{owner = order id, id_type 2, amount = amountLeft, is_minter 0}`, no prefix)
   with a covenant binding to the order's `tokenCovId`. KAS-holding kinds must not have one.
6. Pair orders (`0x08`, `0x09`, `0x0a`): the record's family is the family of A; each of A and B is a supported token
   program of the family the state names for it, whose prefix and suffix lengths are the ones the state records; A and B
   are different tokens (covenant ids); a KRON token carries no extension commitment (its field zero); a KRON amount the
   order holds stays within 10^9 base units. Each custody part (the table above) is the P2SH of that token's custody
   state of exactly the amount the state names (`custody`; a sell-first `KobIfdPair`'s A custody: `amountLeft`), owned
   by the order id (KCC-20 scheme 0x04 with the part's extension commitment; KRON `id_type` 2), with a covenant
   binding to that token's covenant id.
7. `deadline`, when present, is informational for matchers (`matcher.md` §2.3); indexers store it
   with the order. It never changes what the covenant accepts (soft expiry).
8. Indexers list an order only if its state passes the numeric gate (`scale` a power of ten, `scale ≤ 10^9`, and the
   full-fill value of every rate it carries below 2^62: `order-types.md`, *Overflow*); builders refuse to create one
   that does not.

Consensus has already accepted the transaction, so the covenant scripts themselves guarantee that
later spends follow the order's rules; the checks above only establish *which* outputs are KOB
orders, what their state is and where their custody lies. From there an indexer follows the
covenant lineage (continuations keep the id; the mutable splice windows are listed in
`matcher.md` §1.1 and `kob_protocol::state::mutable_windows`), independent of later payloads, and
flags any other token UTXO owned by an order id as a stray, and a token UTXO of another token owned by
an order id as a foreign stray (`matcher.md` §1.2). An IOC ask that ends with an unsold amount returns it
at the output with the index of its custody input (`tokOut = tokenIn`).

### Validation of an AMEND record (indexers, `recoverAmends`)

The maker's `cancel` constrains no output: consensus lets the authorising input continue its covenant id into ANY
script. An AMEND record is therefore accepted only against the state the order input spends, which the verifier proves
itself (an indexer: the redeem script the input reveals, hashed to the spent output; `recoverAmends` on a signed
transaction: the same from its signature script; on a built one: the wallet's signing plan). Reject the record unless
all hold (`kob_protocol::payload::verify_amend`):

1. The record decodes (rules 1 and 2 above) and its kind is the plain ask (`KobAsk` / `KobAskKron`) or the plain bid
   (`KobBid` / `KobBidKron`) of a version-4 payload.
2. `inputs[input]` spends that previous state: its UTXO's script is `P2SH(previous)`, it carries a covenant id, its
   entry is `cancel` (the maker's signature, SIGHASH_ALL: only the maker can amend), and no other input carries the id.
3. The amended state keeps the terms above (an ask: maker, token, `scale`, `amountLeft`; a bid: maker, token,
   `extensionCommitment`, `scale`), of the same template; an ask keeps `amountLeft > 0`.
4. `outputs[output].scriptPublicKey == P2SH(prefix ‖ state ‖ suffix)`, bound to the order's covenant id with
   `authorizingInput = input`, and it is the ONLY output of the transaction bound to that id (as rule 4: a sibling
   would carry the id and could unlock the custody outside the order's rules).
5. Indexers also require the amended state to pass their numeric gate and the order's custody to be live after the
   transaction (an amend never moves it), and re-run their listing rules on the new terms.

### SWEEP record (sweep in place) and its validation (`verify_sweep`)

```
output        LEB128    index of the continuation output (the same order)
input         LEB128    index of the order input the maker's cancel spends
```

The maker's cancel continues the covenant id under the SAME script, so the order, its state and its custody are unchanged;
the transaction moves only strays (`matcher.md` §1.2) back to the maker: of the order's own token, of either token of a
pair order, and foreign ones (`kob_protocol::build::SweepOrder`). It works for every order kind and can be repeated (strays of one
token in one transfer share one extension commitment and fit the program's token inputs; the rest goes in the next sweep).
The continuation is a new UTXO: its DAA score restarts the 90-day idle window and the IOC / FOK kill time, as any
continuation does. The record is OPTIONAL: an indexer that does not know it treats the continuation like any unproven
continuation of a cancel (below), never as another order. Reject the record unless all hold:

1. `inputs[input]` spends the previous state (proven as for an AMEND record), carries a covenant id, its entry is
   `cancel`, and no other input carries the id.
2. `outputs[output].scriptPublicKey` equals the spent UTXO's script (`P2SH(previous)`), bound to the order's covenant id
   with `authorizingInput = input`, and it is the ONLY output bound to that id.
3. Indexers also require the order's custody (ask-side kinds with an amount left) to be live after the transaction.

Any other continuation of a maker's cancel (no record, a record that fails, a second output carrying the id) is
unproven: its state is unknown, and conforming indexers list it nowhere (not in the matcher's book, the public book,
depth or counts). A fill racing an amend spends the same order outpoint: consensus accepts one of the two, and a fill
built on the old state cannot spend the continuation (its redeem script no longer hashes to the order's script).

The legacy x402 text payload `X402:<hex>` is accepted by the decoder as a single X402 record
(`legacy: true`); new clients write a KOB1 payload with an X402 record instead.

## Change (amend builders)

A change output whose own storage mass (KIP-9: `10^12 / value`) would exceed the transaction's fee mass (`max(compute,
2 × bytes)`) is *tiny*: it costs no relay fee but makes storage the transaction's largest mass (a storage-inclusive
fee pays for it, a block holds few such transactions), and returns less than it costs. The amend builders (cancel-replace
and in-place amend, `kob_protocol::build::absorber_for`) never create one: a tiny change rides on the maker's new order
instead (on a plain ask's carrier, or a plain bid's escrow while it does not change the bid's buying power, the largest
fill its escrow funds at the budget rate), and the maker gets it back with the order. Every other kind keeps the change
output. A genesis's covenant id is keyed by its output value, so a replacement that takes a change has its id (and its
custody's owner) re-derived. An in-place amend without funding pays its fee from the order's carrier (no change at all),
leaving at least `max(0.1 KAS, refundTip)` on the order.

## Sizes and cost

Measured on the version-4 records (`crates/kob-protocol/tests/cost_table.rs`, payload column; the pair rows: the golden
vectors `pair.create.*`).

| Transaction | Payload bytes (version 4) |
|---|---|
| Create ask / conditional ask / sell-first IFD (KCC-20, a non-zero extension commitment) | 175 / 189 / 509 |
| Create bid / conditional bid / buy-first IFD | 192 / 194 / 467 |
| Same, KRON (ask / conditional ask / sell-first IFD; bid / conditional bid / buy-first IFD) | 143 / 156 / 443; 160 / 161 / 434 |
| A KCC-20 ask whose token has a zero extension commitment | 32 less than the KCC-20 row |
| Create a pair order: `KobPair` ask / bid, `KobCondPair` ask / bid, `KobIfdPair` buy-first / sell-first (two custody parts); KCC-20 A and B | 280 / 280, 294 / 294, 756 / 790 |
| In-place amend of an ask (either family) | 143 |
| In-place amend of a bid: KCC-20 / KRON | 179 / 147 |
| Day order: + deadline | + 5 (version 2: + 8) |
| x402 reference only (32 B) + note | 56 (version 2, unchanged) |

Each payload byte adds one byte to the transaction, so 2 grams to the fee mass (transient) and
1 gram of compute mass: a record costs twice its size in grams of fee mass (the version-4 ask record, 175 payload bytes:
350 grams, 0.00035 KAS at the relay floor).

## Examples

Hex of the `create.ask` vector (175 bytes), split at the fields:

```
4b4f4231 04                      magic, version 4
01 a700                          ORDER, length 167 (u16le)
00 01 01 00                      output 0, family KCC-20, kind KobAsk, flags 0
00                               zeroMask: no all-zero 32-byte field
1b84c556…  70707070…  f4ac029d…  maker, tokenCovId, tokenTplHash (32 bytes each)
01 a117 e807 e807 …              tplPrefixLen 1, tplSuffixLen 2977, scale 1000, minFill 1000, price, tip, ... (LEB128)
01 eeee…ee                       custody: token output 1, extension commitment (flag bit 1 clear)
```

`create.ask.day` sets flag bit 0 and appends the LEB128 deadline `80a2f1d506` (1,790,726,400 = the next 00:00 UTC).

`amend.ask`:

```
4b4f4231 04
03 8700                          AMEND, length 135 (u16le)
00 00 01 01 00                   output 0, input 0, family KCC-20, kind KobAsk, flags 0
00 1b84c556… …                   the amended state, compact (a new price)
```

`x402.only` (version 2: no order record):

```
4b4f4231 02
81 2000 abab…ab                  X402, 32-byte reference
82 0d00 783430322d636c69656e742f31   NOTE "x402-client/1"
```
