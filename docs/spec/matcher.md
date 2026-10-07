# KOB matcher / executor specification (protocol v3)

Status: normative for `kob-executor` and for KOB wallets. Contracts: `contracts/v2/*.sil` (protocol v3). The order
types and their user-visible semantics are listed in `order-types.md`; amounts (base units), prices (quote token per
whole base token), the rounding rule (`quoteOf`) and the minimum fill (`minFill`) are defined in `order-types.md`,
*Amounts, prices and rounding*, and every rule below uses them. The key words MUST, MUST NOT, SHOULD and MAY are used
as in RFC 2119.

The covenants enforce every price, quantity, custody and trigger rule on chain. A matcher that
breaks this specification cannot steal or overcharge anyone; its transactions are simply invalid
or unprofitable. This document defines what an honest, conforming matcher builds, so that every
retail order type behaves as users expect: which orders it selects, in which order, under which
hard constraints, and how triggers, trailing, if-done exits and chaining are handled.

Roles. A **matcher** builds batches of crossing orders and earns the crossing spread plus tips. It also arms and trails
stops, inside the batches that fill their trigger evidence (§4). A **keeper** refunds orders for their `refundTip`. One
`kob-executor` process may play both roles. Neither holds user funds.

---

## 1. Indexing

### 1.1 Placement record (order visibility)

Order UTXOs are P2SH: the redeem script, and so the order's parameters, is not on chain until the
order is spent. A KOB wallet MUST therefore publish, in the payload of every transaction that
creates an order (genesis, including the replacement of a cancel-replace), a placement record per
order output. The placement record is the `ORDER` record of the KOB1 payload; its byte encoding
and validation rules are defined in `kob1-payload.md` (payload version 4; an order under a template this build does not
pin is unknown: `template-retirement.md`), and it holds:

| Field | Content |
|---|---|
| output | the order UTXO's output index |
| family, kind | token family (0x01 KCC-20, 0x02 KRON) and the kind code, which name the pinned template of `KobAsk`, `KobBid`, `KobCondAsk`, `KobCondBid`, `KobIfdBid`, `KobIfdAsk` (in a KRON record the `…Kron` template of the same name), `KobPair`, `KobCondPair` or `KobIfdPair` (one template each for both families: the family of the record is that of the pair's base token A) |
| state | the order's full state span bytes |
| custody | ask-side kinds (`KobAsk`, `KobCondAsk`, `KobIfdAsk`): the output index (KCC-20: and extension commitment) of the custody token UTXO; its amount is not recorded, it is `amountLeft` (§1.2). Pair orders: one part per custody the state holds, each in the family of its own token: the custody of S (`KobPair`, `KobCondPair`: exactly `custody`; a buy-first `KobIfdPair`'s B escrow), a sell-first `KobIfdPair`'s A custody (exactly `amountLeft`) followed by its B prefund custody (exactly `custody`, present when non-zero) |
| deadline | optional: wall-clock deadline (UTC, unix seconds) of a day order (§10.10) |

The indexer MUST recompute `P2SH(template prefix ‖ state ‖ template suffix)` and list the order
only when it equals the output's script public key, the output is a fresh covenant whose id is the
consensus genesis id of a group that is this output alone (no other output is bound to the id), the state passes the
numeric gate (`scale` and the full-fill values, `order-types.md`, *Overflow*), and every custody output is the token
program's P2SH of exactly the amount the state says (ask side: `amountLeft`; pair orders: per custody, above) owned by
that id (`kob1-payload.md`, *Validation*; `kob_protocol::payload::recover_orders`).
An order without a valid placement record is invisible; a matcher MUST NOT guess parameters.

Continuations need no new record. The indexer derives them from the spending transaction's
signature script (the redeem script is the last push) and the documented splice windows (checked
against the templates by `kob_protocol::state::mutable_windows`, wasm `mutableWindows`):

| Kind | Mutable fields (bytecode payload windows) |
|---|---|
| `KobAsk` | `amountLeft` [236..244) |
| `KobBid` | none (budget = UTXO value) |
| `KobCondAsk` | `stopPrice` [182..190), `armed` [245..253), `amountLeft` [272..280) |
| `KobCondBid` | `stopPrice` [224..232), `amountLeft` [287..295), `armed` [296..304) |
| `KobIfdBid` | `amountLeft` [161..169), `armed` [287..295), `rptAmount` [296..304) |
| `KobIfdAsk` | `armed` [245..253), `amountLeft` [254..262), `rptAmount` [263..271) |
| `KobPair` (both families) | `amountLeft` [398..406), `custody` [407..415) |
| `KobCondPair` (both families) | `stopPrice` [416..424), `armed` [425..433), `amountLeft` [434..442), `custody` [443..451) |
| `KobIfdPair` (both families) | `armed` [440..448), `amountLeft` [449..457), `custody` [458..466), `rptAmount` [467..475) |

The KRON kinds (`…Kron`) have no token extension commitment: `KobAskKron`, `KobCondAskKron` and `KobIfdAskKron` have
the windows above unchanged, and the bid-side kinds sit 33 bytes earlier:

| Kind | Mutable fields (bytecode payload windows) |
|---|---|
| `KobCondBidKron` | `stopPrice` [191..199), `amountLeft` [254..262), `armed` [263..271) |
| `KobIfdBidKron` | `amountLeft` [128..136), `armed` [254..262), `rptAmount` [263..271) |

**In-place amend.** A maker's `cancel` constrains no output, so it can continue the order's covenant id. A plain ask
(`KobAsk` / `KobAskKron`) amended that way keeps its id and its custody (which never moves: no token program is revealed, so an amend is far cheaper than a cancel-replace, §9) and announces its new state in an `AMEND` record
(`kob1-payload.md`). A plain bid (`KobBid` / `KobBidKron`) is amended the same way: it owns no custody, its escrow on
the continuation is its quantity. The indexer accepts an amend only against the previous state the input reveals: the
entry is `cancel`, the new state keeps the maker, the token, `scale` and (an ask) `amountLeft` (so the custody stays
exactly `amountLeft`; a bid also keeps the `extensionCommitment` of its deliveries), the output is the P2SH of that
state and the only output carrying the id, the state passes the numeric gate and (an ask) the custody is still live;
then the order takes the new terms (price, tip, minimum fill, time in force, expiry, activation, deadline; a bid also
its reserve, carrier and escrow, from which its buying power follows), the listing rules run again on them, and an
`amend` event records the replaced terms (`order_amends` keeps them for a reorg).
Its exposure and age restart from the continuation's UTXO DAA, as for any new UTXO (the touch rule §4.1 reads the UTXO
DAA as well as the custody's). Any other continuation of a cancel (no record, a failing record, a second output with
the id) is unproven: listed nowhere, in no book, depth or count.

If-done exits are derived from the entry: the exit's state is the committed `exitState` with
`amountLeft := n` (a pair exit also `custody :=` its custody: n of A for a buy-first entry, the proceeds plus the
prefund of the fill for a sell-first one, read from the fill's arguments), followed by the repeat fields the entry writes
itself (`parent`, `rptPrice`, [`rptPre`,] `rptUntil`: zero for a plain exit, §6.1); its covenant id is the genesis id of
the entry's fill output (§6). The repeat fields of an exit are immutable (bytecode `KobCondAsk`
[280..331), `KobCondBid` [322..382); KRON `KobCondAskKron` [280..331), `KobCondBidKron` [289..349); `KobCondPair`
[451..511)).

### 1.2 Custody validation

An ask-side order (`KobAsk`, `KobCondAsk`, `KobIfdAsk`) is listable only while exactly one live
token UTXO owned by its covenant id (KCC-20 owner scheme 0x04; KRON `id_type` 2, `is_minter` 0) holds exactly `amountLeft`
base units. Any other token UTXO owned by an order id (sent outside the protocol) is a **stray**:

- the indexer MUST flag strays and MUST NOT count them as liquidity;
- a matcher or keeper MUST NOT put a stray in any transaction (the covenants refuse it: the fill,
  settle and refund paths scan the token inputs, the update paths refuse any token input owned by
  the order's id);
- the maker's wallet SHOULD offer to sweep strays: with a SWEEP in place (the maker's `cancel` continuing the order under
  the same script, a `SWEEP` record, `kob1-payload.md`: the order lives on unchanged, repeatable until every stray is
  back) or with the `cancel` itself. Only the maker's `cancel` moves them (maker-signed, SIGHASH_ALL). A stray left when
  its order terminates is lost: no input can carry a terminated order's covenant id again, so nothing can authorise it.

These rules cover tokens of the order's OWN token (its `tokenCovId`; a pair order: A and B). A token UTXO of ANY OTHER token
(another covenant id, of any program or family) owned by an order id (KCC-20 owner scheme 0x04, KRON
`id_type` 2) is a **foreign stray** and is NOT protected: its program authorises it
whenever an input carrying the order's covenant id is spent, and an order cannot recognise another
program's state, so every spend of the order (fill, refund, update, cancel) lets whoever builds the
transaction move it. It never affects the order itself (custody, payouts and deliveries are pinned to
the order's own token). The indexer MUST flag foreign strays and never count them (views: `foreign: true`, with the proven state and
program); wallets and builders MUST NOT send any token to an order id; a matcher MAY sweep a foreign stray, and nothing
stops it. The maker's cancel and sweep return them; a keeper MAY return them to the MAKER inside the refund, kill or close
that ends the order (`kob-executor --return-foreign-strays`: no refund path reads another token, and after the order
ends they could never move again).

A bid-side order (`KobBid`, `KobCondBid`, `KobIfdBid`) owns no tokens; every token UTXO owned by
its id is a stray. A pair order owns exactly its custodies (§1.1: the custody of S; a sell-first `KobIfdPair` its A
custody and its B prefund) and is listable only while each of them is live and exact; any other UTXO of A or of B owned
by its id is a stray of that order (every path of the pair templates refuses an input of either token owned by the
order's id other than its own custodies, an update refuses all of them, a `KobPair` refund refuses any input of T),
swept only by the maker's `cancel`. A token of neither A nor B owned by a pair order's id is a foreign stray (below). A
repeating `KobIfdAsk` with `amountLeft = 0` has no custody at all (it waits for its exits, §6.1); every token UTXO owned
by its id is then a stray (a repeating `KobIfdPair` likewise holds only the custodies its state counts).

### 1.2a Token listing: genesis check and warnings

A token's covenant id commits only to the P2SH hashes of its genesis outputs, so a genesis may hide a
non-template output that later mints look-alike tokens or feeds a fake balance into a genuine
transfer; neither the template nor a holder UTXO shows it. The registry records the check of EVERY
genesis output (`kob_protocol::registry::verify_genesis`) as the token field `genesis_verified`
(`true` clean, `false` not clean, absent not checked; `registry/README.md`).

- The indexer MUST attach the warning code `genesis_unverified` (`warnings` of `GET /v1/tokens`
  and of every order view of that token) to every token whose registry entry does not say
  `genesis_verified: true`, including every open-list token without an entry.
- A token without `genesis_verified: true` MUST NOT be shown as `official`; an operator MAY list
  only tokens with `genesis_verified: true`.
- Wallets MUST show the warning (and the entry's free-text `warning`) wherever the token is shown.
- The entry's `genesis` record states what the check found (genesis txid and DAA score, the group's
  outputs and supply, its mint-authority outputs) and whether a live mint authority exists
  (`live_minters`: `[]` none, absent undetermined). `official` additionally needs `live_minters: []`;
  a token with a live mint authority MUST carry a `warning`. Anyone can re-derive the records from
  the committed chain evidence (`kob registry verify-genesis --network mainnet`, evidence collected
  read-only by `web/scripts/registry-genesis-evidence.mjs`); every recorded byte is checked against
  a hash (txid, covenant id, P2SH), so the explorer that serves pruned history is not trusted.

KRON deliveries to makers use `id_type 3` (address presence): the token program accepts any P2PK
input of the owner key as authority for ALL of that key's type-3 balances. The orders' guarantees
therefore assume that makers sign with SIGHASH_ALL only: a KOB wallet and every KOB
signer MUST refuse SIGHASH_SINGLE, SIGHASH_NONE and ANYONECANPAY signatures by a maker key (the
covenants' own `cancel` entries already refuse anything but a 65-byte SIGHASH_ALL signature).

### 1.3 Reorgs

The indexer follows covenant-id lineage reorg-aware (virtual-chain `added` / `removed`). A fill is
final for the book only when accepted plus N DAA (default 100). A matcher MUST re-validate every
input of a pending batch after a reorg and rebuild it; the covenants make every stale batch
invalid, never harmful.

---

## 2. The book

Books are kept per (token covenant id, token template, extension commitment, `scale`). A pair book A/B is kept per
(A, B), each token with its covenant id, template and scale, and holds the pair orders of both sides (`KobPair`,
`KobCondPair`, `KobIfdPair`: asks sell A, bids buy A) at their prices in B per whole A. For every listed order the book
holds its kind, quote function (quote token per whole base token), quantity (base units), minimum fill, tip, time gates
and trigger state.

### 2.1 Quote at time t

`t` is the DAA score the matcher will prove with the transaction's lockTime (CLTV). Every quote is in the order's quote
token per whole base token (`scale` base units): sompi for the KAS books, base units of B per whole A for a pair order.
The rows apply to the pair kinds alike, in B (a `KobPair` ask decays like an ask, a bid rises like a bid; a
`KobCondPair` has the legs of the conditional of its side; a `KobIfdPair` stop entry the entry auction).

| Order | Quote at t |
|---|---|
| plain ask / bid | `price` |
| decaying ask (`slope > 0`) | `max(priceEnd, price − slope·⌊(t − origin)/decayStep⌋)` |
| rising bid (`slope > 0`) | `min(priceEnd, price + slope·⌊(t − origin)/decayStep⌋)` |
| origin (both) | `activeFrom`, or for `interval > 0` (TWAP / DCA) `max(activeFrom, UTXO DAA + interval)`: every slice is its own auction |
| conditional TP / limit leg | `tpPrice` |
| conditional stop leg (armed) | sell: `stop − ⌊stop·bps(t)/10⁴⌋`; buy: `stop + ⌊stop·bps(t)/10⁴⌋` (multiply first, exact at any stop price, rounded in the maker's favour; a stop leg needs `stop ≤ 922,337,203,685,477`); `bps(t) = slipBps` if `bandDaa = 0`, else `slipBps·min(t − o, bandDaa)/bandDaa` with origin `o` = the armed UTXO's DAA (`armed = 1`) or `armed` (≥ 2) |
| conditional stop leg armed in the same transaction | the stop itself (`bps = 0`) when `bandDaa > 0` |
| if-done stop entry (armed) | buy: `entryStop + (price − entryStop)·min(e, bandDaa)/bandDaa`; sell: `entryStop − (entryStop − price)·min(e, bandDaa)/bandDaa`; `e = t − o`, origin `o` as for the stop leg (`bandDaa = 0`: `price` at once) |

All-in, for a fill of n base units at quote p (`quoteOf`, `order-types.md`, *Amounts, prices and rounding*): an ask
receives at least `⌈n·(p − tip)/scale⌉` (the covenant requires `p ≥ tip`), a bid pays at most `⌊n·(p + tip)/scale⌋`
and consumes `⌈n·(pMax + tip)/scale⌉` of its escrow. A pair order trades n base units of A at quote p in B: an ask
receives at least `⌈n·p/scale(A)⌉` of B, a `KobPair` bid pays exactly `⌊n·p/scale(A)⌋` of B (a `KobCondPair` /
`KobIfdPair` bid at most that), and every pair fill releases at most `⌊n·tip/scale(A)⌋` sompi of KAS tip.

### 2.2 Eligibility at t (hard; the covenant rejects otherwise)

- `t ≥ activeFrom` (the lockTime must be ≥ every `activeFrom` and auction `t` used in the batch
  and ≥ `exposedSince + minRestDaa` of every trigger evidence it uses, §4.2, and the inputs need a
  non-final sequence);
- TWAP / DCA: the order UTXO is ≥ `interval` DAA old (CSV), `n ≤ maxFill`;
- stop legs and stop entries: armed, or armed in this very transaction by trigger evidence filled
  in it (§4);
- `n ≤ amountLeft` (asks: the custody holds exactly `amountLeft`); a bid: the consumed budget `⌈n·(pMax + tip)/scale⌉`
  leaves at least `reserve` of its escrow (its remaining buying power, for the book, is the largest n whose budget
  leaves `deliveryCarrier + reserve`);
- a pair order: `n ≤ amountLeft`; its custody holds exactly the state's `custody` (an ask: `amountLeft`); a partial
  `KobPair` fill needs the order UTXO to hold `deliveryCarrier` plus the tip of the fill (the order funds its own carrier
  and tip); the other pair kinds pay `deliveryCarrier` (an entry also `exitCarrier`) and the tip from their UTXO as well,
  and the builders fund nothing else for them;
- **minimum fill**: asks, conditional orders, if-done entries and every pair order `n ≥ minFill` unless `n = amountLeft`;
  a `KobBid` `n ≥ minFill` unless the fill leaves it less than one minimum fill of buying power (it then terminates:
  `left − deliveryCarrier − reserve < ⌈minFill·(pMax + tip)/scale⌉`, `left` = escrow − consumed budget). This bounds a
  non-repeating order to `⌈amount / minFill⌉` fills; a repeating if-done entry re-opens the "takes everything left"
  exception after every merge, so its fills (and exits) can be as small as the re-armed amount (bounded per cycle, not over
  the entry's life). A `KobBid`'s quantity, FOK completeness and GTC end are exact in value terms: it ends with less than
  one minimum fill of buying power left, and that remainder (worth less than one minimum fill plus one sompi) returns to the
  maker;
- FOK: the fill consumes the whole order (ask: `n = amountLeft`; bid: less than one minimum fill of buying power left);
- the order's UTXO appears at most once per transaction (one covenant input per id);
- the transaction carries at most **8 token inputs** of the order's token, KRON 4 (`MAX_TOK_IN` of the family;
  every order refuses more); a pair order scans both of its tokens and refuses more than 8 inputs of either (a KRON
  program accepts fewer);
- KRON: a KRON token input names its authorising covenant input in a one-byte witness read as a signed number, so that
  input's index MUST be below 128 (the builders place it so);
- repeat IFD (§6.1): a take-profit fill of a booked exit (`parent ≠ 0`) MUST spend its entry in
  merge mode in the same transaction, unless `t ≥ rptUntil`; a stop-leg fill of a booked exit
  MUST NOT spend its entry; an entry merges exactly one exit per transaction and is not filled in
  that transaction; an exit's `update` (arm / trail) never runs in a transaction that spends its entry (the exit
  refuses it).

### 2.3 Honest-matcher exclusions (not enforced on chain)

A conforming matcher MUST NOT fill an order whose soft expiry has passed (`t ≥ expiryDaa`, or
the IOC / FOK kill time `max(UTXO DAA, activeFrom) + 600`, or UTXO DAA + 77,760,000), nor a day
order whose placement-record `deadline` has passed by the matcher's UTC clock (§10.10), even if
its `expiryDaa` is still ahead. Those orders belong to the refund keeper (§5). The covenants still
accept such fills at the order's own limit (accepted spec: soft expiry). A merge (§6.1) is not a
fill: it is always allowed.

---

## 3. Batch selection

**Token programs that refuse (frozen / blacklisted).** Every transaction is pre-executed in the engine before it is submitted (section 8).
A plan the TOKEN program rejects is not a planner defect: the order it is attributed to (the custody input that failed, or the fill
whose removal makes the plan pass) is reported to the book source as possibly frozen, excluded from planning for 5 minutes and
probed again; the indexer keeps it out of the books until a probe passes or the order moves. No special mechanism exists in the
covenants: the token program simply rejects.

Per tick (every new block template, default ≤ 1 s), over **every book at once**. A batch is not per token: one
transaction may fill several token books (X/KAS N:M, Y/KAS N:M, …), every order class and the pair orders netted against
each other or routed through them (§3.5). Each token of the transaction keeps its own accounting (its program's leader and slot limits, the
`MAX_TOK_IN` of its family, one extension commitment); the orders of different tokens share only the transaction's size
and mass and the rule of one input per covenant id. When the book set does not fit one transaction, the matcher plans
the next transaction on the book the previous one leaves (§7).

**Normative status.** The covenants enforce only each order's own terms (price, quantity rules, custody, time in force,
triggers); any transaction they accept is a valid fill. The selection rules of this section (priority classes, ranking,
netting and routing of pair orders, the greedy objective) are the reference algorithm of `kob-executor`, not consensus.
Matchers compete, and an operator is free to plan differently; two honest matchers running this algorithm on the same
view build the same batch.

### 3.1 Priority classes

Evaluate in this order; within each book a lower class may only use liquidity the higher classes left:

1. **Immediate orders** received since the last tick: IOC, FOK, market and streaming orders
   (all are `tif` 1 or 2, usually auctions), on a token's KAS book or on a pair (`KobPair` IOC / FOK). They MUST be
   evaluated in the first batch after they become visible (the first transaction of the tick that has room for them).
2. **Triggered orders**: armed stop legs and armed stop entries (their auctions are running), KAS-quoted and pair.
3. **Resting crossings**: every other crossing pair of orders, pair orders included.

Unarmed stop legs and stop entries are not candidates of any class on their own: they become fillable only once the
batch holds qualifying evidence (§4). The reference planner walks them after class 3, at their trigger price, against
what the three classes left; they are never passive liquidity for another order's walk.

Within a class, order candidates by the executable quote at t (best first), then by `tip`
(higher first), then by UTXO age (older first). This price-tip-time order is the norm for
honest matchers; it is not enforced on chain (accepted spec: no on-chain price-time priority). A pair order is ranked in
its pair book by its price in B, and a routed pair order with the orders of the KAS books it routes through at its
implied quotes (§3.5).

The classes are evaluated over all books together: class 1 of every book, then class 2, then class 3. Books compete only
for the room in the transaction: the books are grouped into components (the books of one token, and the two tokens of
every pair book with orders) and, within a class, the components are served in decreasing order of the profit per byte they make on
their own (`Σ spread + Σ tips − fee` of the component alone over its bytes), ties by the smallest token covenant id. The
most profitable part of the book set therefore fills the first transaction; the rest goes into the next ones.

### 3.2 Objective and hard constraints

The income of a batch is primarily the crossing spread; a tip is a priority fee on top of it. Arming and ratcheting
stops (§4.4) are legs of the batch like any other and are charged in the same objective.

Choose the set of fills and updates of all books that maximises `Σ crossing spread + Σ tips + Σ keeper tips of the
updates − network fee` of the transaction subject to:

- every eligibility rule of §2.2;
- **FOK is all-or-none**: a FOK order is either filled completely in this one transaction or not
  included; a candidate set that fills it partially is discarded (the covenant would reject);
- **IOC is maximal**: an IOC order included in a batch SHOULD receive the largest quantity the
  crossing liquidity allows in that transaction (its remainder returns to the maker at once, at
  the output with the index of the ask's custody input: `tokOut = tokenIn`, and
  cannot be filled later);
- per token: the slot limits of its program (3/3 reference, 4/5, 8/8 KOB tokens, 16/16; KRON 4/5) and `MAX_TOK_IN` of
  its family (KCC-20 8, KRON 4), one extension commitment;
- positional outputs (order input i ↔ output i; if-done fills add one exit output; a custody's rest or return at its
  custody input's index);
- per token, conservation: what the transaction's inputs of a token carry equals what its outputs of that token carry
  (the token programs enforce it); a pair order's exact amounts (§3.5) are the builders' to balance;
- trigger evidence (§4): a stop armed or trailed in the transaction reads a qualifying fill of the same transaction (a
  plain KAS-book fill; a pair stop also two of them, or a resting `KobPair` fill, §4.7); an updated order is not also
  filled in it;
- the **physical limits** of a transaction, and nothing else: the block mass limits (compute 500,000, storage 500,000,
  transient 1,000,000, so at most 250,000 bytes at 4 transient grams per byte; rusty-kaspa v2.1.0
  `consensus/core/src/config/params.rs:626`, mempool check `mining/src/mempool/check_transaction_limits.rs:14-44`), at
  most 1,000 inputs and 1,000 outputs (`params.rs:615-616`), a signature script of at most 250,000 bytes
  (`params.rs:25`), a script public key of at most 10,000 bytes (`params.rs:621`), at most 15 signature operations per
  P2SH input for relay (`mining/src/mempool/check_transaction_standard.rs:17`), and the script engine's limits (1,000,000
  bytes of script, 244 stack items, `crypto/txscript/src/lib.rs:77-80`), with the fee of the whole transaction paid at
  the configured rate. v2.1.0 has no separate standard-mass cap below the block limits. A matcher MUST NOT impose a
  smaller size or candidate cap of its own by default: a larger batch only costs its own fee, and a matcher that builds
  too large loses the race; an operator MAY set one (`--max-tx-bytes`, `--max-candidates-per-group`).

Reference algorithm (greedy with FOK repair; exact solvers MAY replace it):

```
candidates ← every order × leg of every book at t (§2), every pair order of every pair book (§3.5)
components ← books joined by token and by pair books, ordered by profit per byte alone (§3.1)
net first (§3.5): in every pair book, the crossing pair asks and pair bids fill against each other at exact amounts;
    their remainders stay candidates for the routes
repeat (quantity repair and route reconciliation):
    alloc ← ∅
    for class in 1, 2: for every order of the class (components in order, earliest visible first, then priority):
        walk the opposite side of its book in priority order, taking every crossing chunk
    for every component in order, every book of it, every resting bid in priority order (and every routed pair
        order's purchase of the token it buys at its rank): walk the book's asks in priority order, taking every
        crossing chunk
    a walk adds a chunk only if every per-token rule, the minimum fill of both orders of the chunk and the physical
        limits still hold; once the transaction has no room for another leg, only the legs already in it are extended
    remove every order left violating its quantity rule (FOK partial, a fill below its minFill that does not take
        everything left); lower the cap of every routed pair order whose two legs disagree (the token it sells
        exact, the token it buys covered) to what both reached
until nothing changes
drop fills whose marginal profit is negative (their chunks' spread and tips < the fee of their bytes and of the
    counterparties only they trade with), several at once and only while that raises the profit of the whole batch
    (tips of the updates and triggered fills a fill's evidence unlocks included), except IOC/FOK of class 1 while the
    batch as a whole stays profitable, never shrinking an IOC's fill; a routed pair order leaves with its route
arm or trail every listed unarmed stop that a fill of the batch qualifies for (a KAS-quoted stop: a plain fill; a pair
    stop: a pair of KAS-book fills of its two tokens, or a resting `KobPair` fill of its pair, §4.7), inside its own fill
    when it crosses, otherwise as an update next to that evidence (§4.4); an update is a leg like any other (its marginal fee is
    charged, its keeperTip, possibly 0, is income) and is dropped only while the batch would otherwise fall below the
    minimum profit, the worst keeperTip minus fee first; a batch whose fills pay no spread (a standalone arming
    transaction) keeps an update only if its keeperTip covers the update's fee
```

The walks are sorted once per plan and bounded by the tick's wall-clock budget (the defence against a hostile book); no
candidate cap is needed (a book of 10,000 orders per side is planned in tens of milliseconds, §9). The result is a
pure function of the view, `t` and the configuration up to the deadline, so two honest matchers with the same view build the same batch.

### 3.3 Auctions (market, pair market, marketable limit, stop, stop entry, TWAP / DCA slices)

For an auction order the matcher MUST pick `t = lockTime = current DAA score − safety margin`
(default 5 DAA) and fill at the earliest tick at which the cross is profitable. Understating `t`
only raises an ask's (lowers a bid's) quote and is self-defeating. Competition between matchers
is what gives the maker the market price instead of the bound. A pair market order (a `KobPair` IOC auction, decaying
for an ask, rising for a bid) is the same: its quote at `t` (§2.1) prices it, so it enters the batch at the first tick at
which an opposite pair order or its route through the two KAS books pays (§3.2); an ask receives at least
`⌈n·p(t)/scale(A)⌉` of B, a bid pays exactly `⌊n·p(t)/scale(A)⌋`.

### 3.4 Oversize FOK and IOC

A FOK that cannot complete within one transaction (counterparties > the token's slot limit, or
more than 8 token inputs, KRON 4, or mass) is never filled; the wallet rejects it at placement (§10.4).
An IOC larger than one transaction gets the largest single-transaction fill; it is not chained.

### 3.5 Pair orders: netting, routing and prices

A pair order (`KobPair`, `KobCondPair`, `KobIfdPair` of a pair A/B, `order-types.md`, *Pair orders*) enforces only its own
guarantees (`contracts/v2/KobPair.sil` header): what it receives and pays (an ask at least `⌈n·p/scale(A)⌉` of B for
exactly n of A; a `KobPair` bid exactly `⌊n·p/scale(A)⌋` of B for exactly n of A, a `KobCondPair` / `KobIfdPair` bid at
most that), the exact amounts of the tokens it holds, the stray guards of both tokens, its positional outputs (the maker's
token at the order's input index, the custody rest or return at the custody input's index, the continuation the only
output carrying its id), its minimum fill, time and KAS rules. It does not constrain where the other side of the trade
comes from.

*Decision 2026-10-05 (founder): no route enforcement. This reverses the earlier rule of the cross limit (`KobCross`), which
accepted token B only from plain asks of B or from one opposite cross limit and delivered token A only to plain bids of A
or to that opposite cross limit.*

**Settlement.** Each token of a transaction is conserved (its program enforces it); within that, the counterparty side of
a pair fill may be any mix of:

- **route**: through the KAS books of both tokens. A pair ask's A is sold to `KobBid`s of A and its B bought from
  `KobAsk`s of B; a pair bid's B is sold to `KobBid`s of B and its A bought from `KobAsk`s of A. The matcher earns the KAS
  spreads of the legs and the tips;
- **netting**: opposite pair orders of the same pair, any number per side in one transaction: the A the asks release
  goes to the bids' deliveries, the B the bids release to the asks' deliveries;
- **inventory**: the matcher's own tokens (taker tokens, owned by a key that signs).

Pair asks `i` (fills `nᵢ` of A at quotes `pᵢ`) and pair bids `j` (fills `mⱼ` of A at quotes `qⱼ`) of one pair net in a
transaction without other legs when `Σ nᵢ = Σ mⱼ` and `Σⱼ ⌊mⱼ·qⱼ/scale(A)⌋ ≥ Σᵢ ⌈nᵢ·pᵢ/scale(A)⌉` (each order's own
rounding and minimum fill; a `KobCondPair` / `KobIfdPair` bid may release less than its floor, a `KobPair` bid releases
exactly it). For one ask and one bid of n that is `⌊n·q/scale(A)⌋ ≥ ⌈n·p/scale(A)⌉`: `q ≥ p` is necessary, not sufficient
(at `q = p` the two roundings leave a gap unless `n·p` is a multiple of `scale(A)`). Route legs and inventory may cover
any difference. A token surplus (the bids release more B than the asks need, or a KAS ask's minimum fill buys more of a
token than the pair legs take) goes to the delivery of a pair ask buying that token (its guarantee is a floor; the
reference builders use the batch's first such ask), or is sold into `KobBid`s of that token in the same transaction, or
goes to the batch's taker (`kob_protocol::build`, pair legs).

**Implied quotes.** A routed pair order competes for the liquidity of the two KAS books with the orders of its class,
ranked by price → tip → age at its implied KAS quotes. With the best plain all-in quotes at `t` (sompi per whole token)
`askA`, `bidA` of A and `askB`, `bidB` of B (reference: `kob-executor`):

- a pair **ask** (sells A for B at `p(t)`) is an ask of A at `⌈p(t)·askB/scale(B)⌉ − tip` (the KAS its B costs, less
  its KAS tip, per whole A) and a bid of B at `⌊bidA·scale(B)/p(t)⌋` (the KAS one whole A fetches, per whole B); it
  buys exact base units of B from plain asks of B (each in any amount of at least its own `minFill`, or all it has
  left) until `⌈n·p(t)/scale(A)⌉` is covered;
- a pair **bid** (sells B for A at `p(t)`) is a bid of A at `⌊p(t)·bidB/scale(B)⌋ + tip` (the KAS its B fetches, plus
  its KAS tip, per whole A) and an ask of B at `⌈askA·scale(B)/p(t)⌉` (the KAS one whole A costs, per whole B); it sells
  exactly `⌊n·p(t)/scale(A)⌋` of B into plain bids of B.

Each rounding is conservative for the route (costs up, income down); the quotes only rank, and the profit test is
§3.2's. The batch reconciles the two legs to one fill `n` of the pair order (the token it sells exact, the token it buys
covered: the lower of what both legs reached; caps only decrease), and the marginal-profit step of §3.2 drops a route
whose `Σ bid all-in − Σ ask all-in` does not pay its bytes. Ties: price → tip (sompi per whole A) → UTXO age → covenant
id.

**Reference planner** (`kob-executor`; pure functions of the view, none changes what a valid transaction is):

- **net first.** Before the walks, in every pair book, the planner fills the crossing pair asks (lowest price first,
  then tip, then age) against the crossing pair bids (highest first) at exact amounts, any number of orders per side in
  one transaction, while the B the bids release covers the B the asks need and every order's minimum fill and quantity
  rule holds, within the token rules below. The remainders stay candidates.
- **then route.** Every remainder, and every pair order without an opposite, is routed through the KAS books like any
  other order, in the same transaction where the token rules allow, otherwise in the next one.
- **no inventory by default; surplus inventory opt-in.** The reference planner sells no tokens of its own: it fills pair
  orders only by netting and routing. With its surplus-inventory policy off (the default) it also keeps none: a token
  surplus goes to a pair ask's delivery unless KAS bids take it in the same transaction. *Owner decision 2026-10-06:* an
  operator may switch the policy on (`PlannerConfig::inventory`, `docs/ops/executor.md`, "Surplus inventory"). A
  surplus that no KAS bid takes in the same transaction (preferred: no inventory risk) then goes to the batch's taker,
  the operator's key (`Batch::keepSurplus`), instead of the pair ask's delivery, and its value counts as income of the
  batch, so a crossed pair match that pays no KAS (no tips, the surplus below every bid's minimum fill, valued at the
  owner's `refPrice`) still pays its fee. The pair ask receives exactly its floor `⌈n·p/scale(A)⌉`, which is all its
  covenant guarantees (`KobPair.sil` `tOut >= ceil`, and `KobCondPair.sil` likewise; a `KobIfdPair` exit custody is exact
  and never takes a surplus). Only tokens on the operator's allowlist are kept. The value is the owner's `refPrice` if set
  (the owner sells the inventory off-matcher); otherwise it is what the plain resting KAS bids of the token pay for the
  amount (best first, each only up to what it holds and only for an amount its own quantity rules accept: at least its
  minimum fill, or all it has left; a bid that could never take the amount does not value it, whatever it quotes; never
  the best bid's quote alone). The value is then cut by `haircutBps` (default 80 %); it must exceed the fee of the
  operator's token output, and the batch as a whole still needs `min_profit`. Without `refPrice` the amount must be at
  least the smallest minimum fill of the valued bids (no unsellable dust); `minAmount` may raise that bound, never lower
  it. The matcher only accumulates: the reference executor's maintenance jobs never sell a listed token (they may merge
  its UTXOs).
  *Carrier of the kept output* (`keepCarrier`, `Batch::keepCarrier`; owner decision 2026-10-06: small enough to cost
  nothing): the operator's inventory output carries 2 KAS by default, not the 10 KAS of its other token outputs; the
  carriers of every order output (deliveries, custodies, exits) are the orders' own terms and do not change. A token
  output carries a covenant id, so its KIP-9 storage plurality is 2 (rusty-kaspa `utxo_plurality`, 100-byte units) and
  its storage mass is `4 × 10^12 / carrier` grams. The relay fee (`rate × max(compute, 2 × bytes)`) does not price
  storage mass, so the fee is the same at every carrier the block storage limit allows (above 0.08 KAS); the
  storage-inclusive priority mass is unchanged while the storage mass stays below the batch's fee mass. Measured
  (`crates/kob-executor/tests/matcher_crossmatch.rs`, `the_kept_output_carrier_adds_no_fee`), 10 KAS against 2 KAS: the
  zero-tip TBTC/TUSD (8/8) keep batch 20,823 bytes, fee 0.041646 KAS both, storage mass 4,259 → 20,217 below its fee
  mass 41,646; the smallest keep batch, a KRON / KRON 1 × 1 netting, 11,773 bytes, fee mass 23,546, storage 20,217 at
  2 KAS but 26,881 at 1.5 KAS (above the fee mass: a higher priority fee). 2 KAS is therefore the smallest round carrier
  that adds no fee in either mode for every program pair. The policy refuses less than 0.5 KAS (KaspaCom KCC20 0.2.5
  refuses a token output below it); the operator's later sale of the output only spends it (a small input never adds
  storage mass).
- **standalone bound.** Before the walks, each routed pair order is capped at the largest fill its route could pay on its
  own and is left out of the batch when none can: the token it buys taken along the plain asks of that token (best first;
  an ask's minimum fill counted in full, its surplus going to the maker), the token it sells along the plain bids of that
  token (best first), plus the tips, less the fee of the order's own bytes, against `min_profit`. The estimate is
  optimistic (no slot caps), so it never cuts off a fill that pays. A FOK pair order is judged at its whole size only, and
  a pair market order whose auction still moves at the largest fill the books reach is never cut (it waits for the quote
  at which the route pays it). An IOC pair order thus gets the largest fill that pays rather than none.
- **twin rule.** A leg of a routed pair order enters the transaction only while its other leg can still follow under the
  token rules; the reconciliation drops both legs together.
- **substitutes.** When the profit step drops a losing routed pair order and another pair order of the same tokens only
  takes its place on the same liquidity at a loss, those are dropped with it, all of them (each pass drops at least one
  more); every allocation is settled afresh from what the profit step dropped.

Per transaction a matcher MUST keep (the covenants enforce the amounts, custodies, strays and positions; the builders
refuse every item):

- every pair order's exact amounts: an ask releases exactly n of A and its delivery holds at least its ceil of B; a
  `KobPair` bid releases exactly its floor of B and receives exactly n of A; custodies exact before and after;
- the positional outputs of every pair order (its input index, its custody input's index; an if-done entry's exit output)
  and of every KAS-book order of the transaction: no output serves two orders;
- no input of A or B owned by a pair order other than its own custodies (strays, §1.2);
- per token: at most 8 token inputs (KRON programs fewer), the program's slots, one extension commitment; KRON
  authorising inputs below index 128 (§2.2); a 2 × 2 net or a sell-first exit's partial re-arm needs 4 outputs of one
  token, so a 3-slot program (the 3/3 reference, P2) cannot carry it;
- every order's minimum fill; FOK all-or-none; an IOC pair order SHOULD get the largest fill the transaction allows;
  §2.2 eligibility and the §2.3 exclusions apply to every pair order and every KAS-book leg;
- a pair fill is trigger evidence only of the pair conditionals of the same pair (a resting `KobPair`, §4.7), never of a
  KAS-quoted order;
- **profit:** the transaction as a whole (§3.2); the fee is the node floor of the built transaction; every maker's
  delivery carrier comes out of the order's own prefund.

**Prices and volume (normative for indexers).** Trades, candles, the last price and the statistics of a token are per
token in KAS and come ONLY from fills of KAS-book orders (`KobAsk`, `KobBid` and the KAS-quoted conditional and if-done
kinds, both families). A pair order's fill records a fill event and pair volume (A and B traded). When it is routed, its
KAS-book counterparties' fills are KAS trades as usual; a pair fill whose counterparty is not a KAS-book order (netting,
inventory) sets no price, no candle and no last price, and the read API flags it as volume only, with how the
transaction settled it (`docs/ops/executor.md`). A pair's chart (B per A) is derived from the two KAS series. A resting
`KobPair` fill may arm a pair stop (§4.7), but its price never enters the price record.

Every other book of the batch trades freely next to the pair orders. The pair view A/B shows the pair orders (asks at
`price / scale(B)` whole B per whole A, a decaying ask at its current price; bids likewise) and the route levels implied
by the two KAS books (`docs/ops/executor.md`: `GET /v1/pairs/{a}/{b}/book`).

**x402 swap-and-pay.** A swap-and-pay route (`x402-swap-and-pay.md`) is built and signed by its payer (SIGHASH_ALL over
the whole transaction) and submitted by the facilitator; a matcher cannot fold it into its own batch. A matcher that
runs the facilitator in the same process treats the order outpoints of every settlement in flight (`pending`,
`broadcast`, `ambiguous`) like those of its own pending transactions: its batches plan around them, so a signed route
is never raced by the operator's own batch.

---

## 4. Triggers

A stop leg (stop, stop-limit, stop-market, the stop leg of an OCO: `KobCondAsk`, `KobCondBid`), a
trailing ratchet and a stop entry (`KobIfdBid` buy-stop, `KobIfdAsk` sell-stop), in both families,
change state only on **trigger evidence**: the fill of a plain resting order in the same transaction
(touch trigger). The covenant reads the evidence itself; there is no receipt and no matcher statement, and nothing
persists after the transaction. The pair conditionals (`KobCondPair` stop legs and trailing ratchets, `KobIfdPair` stop
entries) follow the same rules with their own evidence: two plain resting KAS-book fills, one of each token, or a
resting `KobPair` fill of the same pair (§4.7). §4.1 to §4.6 state the KAS-quoted rule; §4.3 to §4.6 apply to both.

### 4.1 The touch rule

An unarmed stop arms, and a trailing stop ratchets, only in a transaction that FILLS (`n > 0`) a
plain resting `KobAsk` / `KobBid` of the same token (covenant id) and `scale` that:

1. was exposed at its quote for at least `R = minRestDaa` DAA before the transaction (§4.2);
2. does not decay (`slope = 0`): the price of a decaying ask or rising bid is not a quote;
3. quotes beyond the stop by the side rule below;
4. trades `n ≥ minTouch` base units (the evidence's fill against the stop's threshold, §4.3).

| Order | Evidence (filled in the transaction) | Quote | Effect |
|---|---|---|---|
| sell stop leg (`KobCondAsk` leg 1) | a resting **ask** | `≤ stopPrice` | arm |
| buy stop leg (`KobCondBid` leg 1) | a resting **bid** | `≥ stopPrice` | arm |
| trailing sell stop (`KobCondAsk`, `trailStep > 0`) | a resting **bid** | `rp ≥ stop + step + gap` | ratchet up (§4.5) |
| trailing buy stop (`KobCondBid`, `trailStep > 0`) | a resting **ask** | `rp ≤ stop − step − gap` | ratchet down (§4.5) |
| sell-stop entry (`KobIfdAsk`, `entryStop > 0`) | a resting ask | `≤ entryStop` | arm |
| buy-stop entry (`KobIfdBid`, `entryStop > 0`) | a resting bid | `≥ entryStop` | arm |

The evidence price is the resting order's quote (`price`): a resting ask sells at its quote or
better for the seller, a resting bid buys at its quote or better for the buyer, so the quote is the
conservative print in both directions. A stop's own counterparty is never its evidence: a sell stop
fills against buyers and its evidence is a seller (a resting ask); a buy stop mirrors it.

### 4.2 Evidence

- **What counts.** Only a plain `KobAsk` / `KobBid` of the stop's family running its fill entry
  with `n > 0`. The evidence templates (hash, prefix and suffix length of `KobAsk` / `KobBid` of the
  family) are build constants inlined in the conditional and if-done templates, the same on every
  network. The input is authenticated like every covenant-to-covenant read (template hash + P2SH of
  its redeem script), and its signature script must start with the fixed 8-byte push of its fill
  argument `n > 0` (only the fill entries of `KobAsk` / `KobBid` start with an 8-byte item, and that
  order's own script enforces the fill for that `n`). A refund, a cancel, and the fills of conditional (`KobCond*`),
  if-done (`KobIfd*`) and pair orders (`KobPair` included) are never evidence of a KAS-quoted order.
- **Covenant arguments.** `ev` is the input index of the evidence order; `tk` (ask evidence) is the
  input index of the ask's custody token UTXO: the input of the stop's token owned by the ask's
  covenant id (KCC-20 owner scheme 0x04, KRON `id_type` 2), whose DAA score the exposure reads.
  `KobCondAsk.update(ev, tk)` and `KobCondBid.update(ev, tk)` read an ask for `tk ≥ 0` and a bid for
  `tk = −1`; `KobCondAsk.settle` and `KobIfdAsk.settle` / `update` take `(ev, tk)` of an ask;
  `KobCondBid.settle`, `KobIfdBid.fill` and `KobIfdBid.update` take `ev` of a bid. The builders
  take the index of a plain leg of the batch (`Leg` field `evidence`, `BatchUpdate.evidence`) and
  derive `ev` / `tk` (`kob_protocol::build::touch_of`, `Touch`). The pair conditionals take `(evA, evB, tk, evMode)`
  (§4.7; builders: `evidence` and `evidenceB`).
- **Exposure.** `exposedSince = max(evidence UTXO DAA + its interval (TWAP / DCA, else 0), its
  custody UTXO DAA (ask), its activeFrom)`. The covenant requires `tx.daa ≥ exposedSince +
  minRestDaa`, `tx.daa` proven by CLTV against the lock time, so the lock time must reach that bound
  (§2.2). Evidence that is itself an unaccepted continuation is not chain-safe (§7).
- **Book.** The covenant needs the same token covenant id and `scale` (prices are comparable only at the same
  `scale`); the reference planner takes evidence only from the stop's own book (same token, token program, extension
  commitment and `scale`), which is the same set in practice.
- **Amount.** The evidence fill `n` (base units of the token; evidence of another token or `scale` is refused) must be
  ≥ the stop's `minTouch` (base units).
- **Sharing.** The evidence is read, never consumed: any number of stops may read one fill of the
  transaction (an arm-in-fill and several updates alike).

### 4.3 Rest time R and threshold

- `R = minRestDaa`, per order; wallet default 50 DAA (5 s, `kob_protocol::defaults::MIN_REST_DAA`).
- `minTouch`, per order, in base units, chosen by the user: the smallest evidence fill that arms or trails.
  The wallet default is the order's own minimum fill (`minTouch = minFill`), and the order ticket offers the presets
  min fill / 25% / 50% / 100% of the order's own amount (rounded up to a base unit, at least 1) plus a custom amount
  (§10.6). The trade-off: a
  smaller threshold triggers sooner but is easier to hunt; 100% is the strongest protection against stop
  hunting, because a hunter must expose at least the size of the order beyond its stop, but a large stop
  may trigger later in a thin market.
- Once armed, a stop is only a limit bounded by its band (§2.1). A quote placed beyond the stop to
  trigger it must rest there for R, open to every matcher, before its fill counts; the threshold
  sets how much of it must trade.

### 4.4 Arming in batches

The evidence exists only in the transaction that fills it, so stops are armed by matchers inside
their batches:

- **Arm in the fill.** The batch fills the unarmed stop leg (`settle`, leg 1) or stop entry at its
  trigger with evidence filled in the same batch. With `bandDaa > 0` it fills at the stop itself
  (`bps = 0`, §2.1), so it needs a counterparty at or beyond the stop.
- **Update.** Otherwise the batch spends the stop in its `update` entry next to the evidence fill
  (`Batch.updates`: the order and the index of its evidence leg). The continuation is armed
  (`armed = 1`, auction origin = its UTXO DAA) or ratcheted, and the batch's change key (the
  matcher) takes up to `keeperTip` from the order's carrier. An updated order is not filled in the
  same transaction; its auction runs from the next one (§3.3). `update` spends no token input owned
  by the order's id. A stop entry's `update` additionally requires `amountLeft > 0` and the stop on the right
  side of the limit (`entryStop ≤ price` for `KobIfdBid` and a buy-first `KobIfdPair`, `entryStop ≥ price` for
  `KobIfdAsk` and a sell-first `KobIfdPair`), the ordering a fill requires as well. A booked exit's `update` never runs
  next to its repeat entry (§6.1, rule 5). The pair kinds have no separate `update` entry: their fill entry runs as an
  update with the 8-byte first argument 0 and `upd = 1` (`KobCondPair.settle(…, upd = 1, k)`, `KobIfdPair.fill(…,
  upd = 1)`).
- A matcher SHOULD arm or ratchet, in each batch, every listed unarmed stop one of its fills
  qualifies for whenever the batch income (the crossing spread plus tips) covers the cost. The `keeperTip` is a
  priority fee, not the price of the update: the update's marginal fee (§9, §10.6) is charged against the batch
  objective (§3.2) like any other leg, and a tip of 0 still arms when the spread pays for it. The reference planner
  includes every qualifying update that fits the transaction's byte budget (the cost of an update is the larger of the
  byte estimate of its input and continuation at the fee rate and the measured marginal `updateFee` of the token
  program, `crates/kob-protocol/data/keeper_tips.json`). It drops updates only while the batch would otherwise fall below
  its minimum profit: the ones with the worst `keeperTip` minus cost first, the later one in tip order (highest tip
  first, then covenant id) on a tie, so two honest matchers drop the same ones.
- **Standalone arming.** A batch whose fills pay no crossing spread exists only to arm; no one else pays for it, so it
  is paid by tips alone: the reference planner keeps an update there only when its `keeperTip` is at least the update's
  cost, and builds the batch only if those tips cover its fee.
- Stops triggered in a batch fill after the resting crossings of that batch (§3.1), and an arm or ratchet never reads a
  fill that is not in the same transaction.
- **Missed batch.** Nothing persists: a batch that fills qualifying evidence without arming a stop
  leaves it unarmed, and the stop waits for the next qualifying fill (by any matcher). No
  transaction records a fill for later use.
- An armed order stays armed; partial fills carry the auction origin. A stop entry armed by `update` (`armed = 1`,
  origin not yet set) with `bandDaa > 0` records the origin (`armed :=` the entry UTXO's DAA) at its first fill or
  merge (§6.1). The TP / limit leg of an OCO is an ordinary resting limit and may fill at any time, armed or not.

### 4.5 Trailing updates

A matcher MAY ratchet a trailing order (`trailStep > 0`, not armed) with `update` next to an
evidence fill of the opposite side (§4.1) when the order UTXO is ≥ `trailWait` DAA old (CSV). The
new stop is fixed by the covenant: every step the evidence justifies, `k = ⌊(rp − gap −
stop)/step⌋` (sell; mirrored for buy), capped below `tpPrice` (sell) or above the limit leg and 0
(buy). The matcher takes at most `keeperTip`, SHOULD ratchet every qualifying trailing order (§4.4) with the most
extreme qualifying fill of its batch, and no more often than `trailWait`. A ratchet is only an update, never part of a
fill of the stop. A pair trailing stop takes k from the filler and the covenant accepts only the valid and maximal k
(§4.7).

### 4.6 What a stop does not do

A stop triggers on a traded resting quote, never on a matcher statement: the earliest possible
trigger is `minRestDaa` (default 5 s) after an order at or beyond the stop started resting there,
and only in a batch that fills at least `minTouch` of it. The evidence may be anyone's order,
the matcher's own included: what makes a false print costly is that it rests beyond the stop, open
to every matcher, for R. Censorship of triggers by every executor for as long as the stop waits is
an accepted residual.

### 4.7 Pair triggers

A pair stop (`KobCondPair` stop leg or trailing ratchet, `KobIfdPair` stop entry) arms or trails only in a transaction
that fills its evidence, in one of two modes chosen by the filler (argument `evMode`; `order-types.md`, *Pair trigger
rule*, has the user-facing statement). Only quotes that cost money to fake count, and the order's own counterparties are
never its evidence (the side rule).

**Mode 0, two KAS books.** Input `evA` is a plain resting `KobAsk` / `KobBid` of A and input `evB` one of B, each of its
token's family, authenticated by template hash (the evidence templates `KobAsk`, `KobAskKron`, `KobBid`, `KobBidKron` are
build constants of both pair conditionals), each running its fill entry (8-byte first push `n > 0`) in this transaction,
each with `slope = 0`, a scale equal to its token's scale (A: the pair's `scale(A)`; B: `scale(B)`) and exposed for at
least `minRestDaa` (§4.2, per order: `max(UTXO DAA + interval, activeFrom, custody DAA)`); `tk` is the custody input of
whichever of the two is an ask (owner = that ask's covenant id with the covenant marker). Thresholds: `n_A ≥ minTouch`
(base units of A) and `n_B ≥ ⌈minTouch·X/scale(A)⌉` (base units of B), X the order's current stop (`stopPrice`, or
`entryStop`). With the quotes `a` of A and `b` of B (sompi per whole token) the implied rate is `r = a·scale(B)/b` B base
units per whole A; the covenant compares exactly through `quoteOf(x, b, scale(B), c)` (no 128-bit arithmetic):

| Order | `evA` / `evB` | Condition (`⇔`) |
|---|---|---|
| sell stop arms (`KobCondPair` ASK leg 1, `KobIfdPair` ASK stop entry) | ask of A / bid of B | `a ≤ ⌊stop·b/scale(B)⌋` (`r ≤ stop`) |
| buy stop arms (`KobCondPair` BID leg 1, `KobIfdPair` BID stop entry) | bid of A / ask of B | `a ≥ ⌈stop·b/scale(B)⌉` (`r ≥ stop`) |
| trailing sell stop ratchets to `s′ = stop + k·step` (`KobCondPair` ASK) | bid of A / ask of B | valid `a ≥ ⌈(s′ + gap)·b/scale(B)⌉` and `s′ < tpPrice` (`tpPrice > 0`); maximal `a < ⌈(s′ + step + gap)·b/scale(B)⌉` or `s′ + step ≥ tpPrice` (`tpPrice > 0`) |
| trailing buy stop ratchets to `s′ = stop − k·step` (`KobCondPair` BID) | ask of A / bid of B | valid `s′ > max(tpPrice, 0)`, `s′ − gap ≥ 0`, `a ≤ ⌊(s′ − gap)·b/scale(B)⌋`; maximal `s′ − step ≤ max(tpPrice, 0)`, or `s′ − step − gap < 0`, or `a > ⌊(s′ − step − gap)·b/scale(B)⌋` |

**Mode 1, a resting pair order.** Input `evA` is a resting `KobPair` of THIS pair (its S and T are A and B in the roles of
its side, with exactly the pair's scales; `evB` unused), of the side named below, authenticated by template hash (`KobPair` is a build constant of both pair conditionals),
running its fill entry with `n ≥ minTouch` (base units of A), `slope = 0`, exposed for at least `minRestDaa`
(`max(its UTXO DAA + interval, activeFrom, its custody's DAA)`, `tk` its custody input); its price `rp` is the rate. A sell
stop arms on a pair ASK with `rp ≤ stop`, a buy stop on a pair BID with `rp ≥ stop`; a trailing sell stop ratchets on a
pair BID and a trailing buy stop on a pair ASK, by the table above with `a = rp`, `b = scale(B)` (then plain comparisons
with `rp`, since `quoteOf(x, scale(B), scale(B), c) = x`). A `KobPair` fill is a real trade at its quote (`order-types.md`,
*Takeable quotes*), so it is evidence; a `KobCondPair` or `KobIfdPair` fill never is.

**Matchers.** The reference planner arms and trails pair stops in the same batch as their evidence (arm in the fill, or
`update` next to it, §4.4): mode 0 from a pair of plain fills of A and B the batch makes anyway, mode 1 from a resting
`KobPair` fill of the batch (a netted or routed one alike). The evidence is read, never consumed (any number of pair
stops may read the same fills), and a mode 1 fill never sets a price (§3.5). A KAS-quoted stop never reads a pair fill.
The other rules of §4.2 to §4.6 (rest time, threshold, missed batches, standalone arming, keeper tips, trailing cadence)
apply unchanged.

---

## 5. Keepers: expiry, kill and refund

- **IOC / FOK kill**: at `max(UTXO DAA, activeFrom) + 600` (or `expiryDaa` if earlier) anyone may
  refund (a `KobPair` IOC / FOK alike). A keeper SHOULD submit the refund in the first block that allows it; the maker's
  wallet SHOULD do it itself when online (it saves the `refundTip`).
- **GTD / day**: refund at `expiryDaa`. **GTC**: refund once idle 90 days (77,760,000 DAA).
- **Repeating entries** (§6.1) are refunded like any if-done entry (`KobIfdBid.refund`,
  `KobIfdAsk.settle(n = 0)` while it holds tokens); a repeating `KobIfdAsk` with `amountLeft = 0` has no
  custody and is refunded by `close()`; a `KobIfdPair` is refunded by `fill(0)` with whatever custodies it holds (none,
  one or two). After the entry's refund its booked exits stay live and take profit without re-arming from their
  `rptUntil` on.
- **Pair orders.** A `KobPair` / `KobCondPair` refund returns the whole custody of S to the maker at the order's input
  index with every carrier (a `KobPair` refund carries no input of T); a `KobIfdPair` refund returns each custody at its
  own input index and the entry's KAS to the maker at the entry's index. Every refund spends the custody inputs of the
  order, so it pushes the token program of each custody it returns.
- Ask-side refunds may be batched (at most 8 token inputs of a token, KRON 4; no strays: each order refuses a
  second token input owned by its own id). Bid-side refunds (`KobBid`, `KobCondBid`, `KobIfdBid`)
  carry no token input at all.
- The keeper takes at most `refundTip`. The fee of a refund depends on the token program (an
  ask-side refund pushes the whole token program in its custody input; a pair refund the program of each custody), so
  wallets MUST set `refundTip` per token program (a pair order: by the programs of its custodies), at least the defaults
  derived from measured fees (`kob_protocol::defaults`, wasm `keeperTips`: 2× the largest fee floor of a funded keeper's
  refund, kill or close, rounded up to 0.001 KAS; `crates/kob-protocol/data/keeper_tips.json`): to be measured (pair
  phase). A flat 0.03 KAS does not pay a 4/5 or 8/8 refund.

---

## 6. If-done sequencing (IFD / IFO / bracket, both sides)

1. An entry fill of n base units (n ≥ `minFill` unless it takes everything left) creates **one exit per fill**:
   a genesis output authorised by the entry input, whose script is `P2SH(exit template prefix ‖
   exitState[amountLeft := n] ‖ repeat fields ‖ suffix)` (repeat fields: §6.1, zero for a plain
   exit) and whose covenant id the entry recomputes. The builder
   MUST compute the genesis id exactly as consensus does (group of one output) and bind the
   output to it.
2. Buy-first (`KobIfdBid`): the n bought base units go to output i, owned by the exit id (scheme 0x04);
   the exit (`KobCondAsk`, `amountLeft = n`) custody is exactly those n base units. The entry pays at most
   `⌊n·(p + tip)/scale⌋`.
   Sell-first (`KobIfdAsk`, which requires `price ≥ tip`): the exit (`KobCondBid`, `amountLeft = n`) holds the
   proceeds `⌈n·(p − tip)/scale⌉` plus the prefund `⌈n·prefund/scale⌉` plus `exitCarrier`.
   Pair entries (`KobIfdPair`; amounts in B per whole A, the tip KAS apart): buy-first (side BID) releases at most
   `⌊n·p/scale(A)⌋` of B from its escrow and delivers exactly n of A at output i, owned by the exit id, as the exit's
   custody (`KobCondPair` ASK, `amountLeft = custody = n`); sell-first (side ASK) releases n of A and puts the proceeds
   `tOut ≥ ⌈n·p/scale(A)⌉` (the fill's argument) plus `pre(n) = ⌈n·prefund/scale(A)⌉` from its prefund custody at output
   i, owned by the exit id, as the exit's custody (`KobCondPair` BID, `amountLeft = n`, `custody = tOut + pre(n)`; the
   last fill of a non-repeating entry hands over the whole prefund left). The exit custody carries `deliveryCarrier`,
   the exit UTXO at least `exitCarrier`.
3. The exit is a normal conditional from the next transaction on (it cannot be spent in the
   transaction that creates it). Its stop leg arms from evidence filled in the arming transaction
   (§4), like any stop.
4. Stop entries (`entryStop > 0`) follow §4.4: armed by a matcher with `update` next to the
   evidence fill, then filled during the auction, or armed inside the fill at `entryStop`.
5. Each exit is independent: the wallet shows one logical position per entry and cancels them in
   one transaction on request (§10.9).

### 6.1 Repeat IFD / IFO, both sides

A repeating entry (`rptAmount > 0`) re-arms its original entry after every take-profit of its
exits, at the same parameters. `rptAmount − 1` is the number of **base units of re-arms** left (the wallet
sets `rptAmount = 1 + K·N` for K repeats of N base units, §10.13); the entry's covenant id is the
position's permanent identity, and every cycle returns to it.

**Booking (entry fill).** A fill of n base units with `rptAmount > n` books its exit (the covenant requires
`n < 2^53`, the merge argument below) and continues with `rptAmount − n`; any other fill creates a plain exit (the last
cycle). The entry writes the exit's repeat fields itself (rates in sompi per whole token):

| Field | Buy-first exit (`KobCondAsk`) | Sell-first exit (`KobCondBid`) |
|---|---|---|
| `parent` | the entry's covenant id | the entry's covenant id |
| `rptPrice` | the entry's budget rate `price + tip` | the entry's proceeds rate `price − tip` |
| `rptPre` | — | the entry's `prefund` |
| `rptUntil` | `min(expiryDaa, max(entry UTXO DAA, t) + 77,760,000)` | same |

`t` is the fill's CLTV-proven time argument (the builder SHOULD pass the lockTime). A repeating
entry never terminates on a fill: sold out, it continues with `amountLeft = 0` (a sell-first entry
then has no custody and keeps the custody carrier in its own UTXO) and waits for its exits.

**Merge (take-profit of a booked exit).** A take-profit fill of m base units by the booked exit at input
k MUST spend its entry in the same transaction in **merge mode**: the entry's `fill` / `settle`
with the 8-byte first argument `−(k·2^53 + m)` (sign-magnitude script number; `k` an input index below 1024,
`0 < m < 2^53`). Both sides check the other's signature script: the exit requires that the entry's starts with the
8-byte push (first byte `0x08`) of exactly `−(k·2^53 + m)` for its own index k and fill m; the entry requires that the
exit's starts with the 8-byte push of its `settle` with `n = m`. The entry also checks that the exit is exactly one it
books: its template, every immutable byte of its state (the committed `exitState` outside the mutable
`stopPrice`, `armed` and `amountLeft` windows: maker, legs, trigger and band rules, carriers), `parent`
= the entry and `rptPrice` (sell-first also `rptPre`) = the entry's own values. A covenant id does
not reveal its genesis outpoint, so a look-alike exit that someone else builds and funds cannot be
told apart; it can only give: it pays this maker on this maker's terms, and the merge returns the
full budget (buy-first) or at least the prefund of m (sell-first, also on a sell-out), so a merge
never costs the entry.

Outputs (every amount rounded in the maker's favour, `order-types.md`, *Amounts, prices and rounding*; the exit and the
entry compute the same rounded values, so no remainder is left between them):

| | Buy-first (`KobIfdBid` + `KobCondAsk`) | Sell-first (`KobIfdAsk` + `KobCondBid`) |
|---|---|---|
| maker (exit output i) | ≥ `⌈m·(tp − tip)/scale⌉ − ⌈m·rptPrice/scale⌉` (the profit) | ≥ `⌈m·rptPrice/scale⌉` − the buy-back spend `⌊m·(tp + tip)/scale⌋` (the profit) |
| entry continuation | same script, `amountLeft + m`, value ≥ in + `⌈m·rptPrice/scale⌉`; on the exit's sell-out also + both exit carriers (checked by the exit) | same script, `amountLeft + m`, value ≥ in + `⌈m·prefund/scale⌉`, or on the exit's sell-out in + max(`⌈m·prefund/scale⌉`, everything the exit held beyond `⌈m·rptPrice/scale⌉`); − `exitCarrier` when a new custody is created |
| tokens | sold to the buyer as usual | the m bought base units go into the entry's custody: exactly `amountLeft + m` owned by the entry id (a NEW custody carrying `exitCarrier` when `amountLeft` was 0; its extension commitment is the exit's) |
| exit continuation | as a plain fill | value ≥ in − `⌈m·rptPrice/scale⌉` − `⌈m·rptPre/scale⌉` |

An empty stop entry (`amountLeft = 0`) re-arms **unarmed**: the new cycle waits for a new trigger
(§4.4). An amount re-armed into an entry that still has some joins its running auction: a merge into an armed entry
that has not filled yet (`armed = 1`) with `bandDaa > 0` records the auction origin (`armed :=` the entry UTXO's DAA)
like a fill, so the merge does not restart the band.

**Pair entries** (`KobIfdPair` + `KobCondPair` exits) repeat the same way; the tip is KAS and outside every rate, so
the entry books `rptPrice = price` (buy-first: the budget rate; sell-first: the proceeds rate), `rptPre = prefund`
(sell-first; 0 buy-first) and the same `rptUntil`. The merge argument, the mutual 8-byte-push checks and the booked-exit
check are those above (the entry compares the committed `exitState` outside the mutable `stopPrice` and `armed`
payloads, the push opcodes of `armed`, `amountLeft` and `custody`, then `parent`, `rptPrice`, `rptPre`, and requires the
opposite side at its own scale of A). Per take-profit of m base units of A (B amounts):

| | Buy-first (`KobIfdPair` BID + `KobCondPair` ASK) | Sell-first (`KobIfdPair` ASK + `KobCondPair` BID) |
|---|---|---|
| maker (exit output i, in B) | `tOut − ⌈m·rptPrice/scale(A)⌉ > 0` (the exit pins it) | `⌈m·rptPrice/scale(A)⌉ − sOut > 0`; on the exit's sell-out `custody − ⌈m·rptPre/scale(A)⌉ − sOut > 0` (its whole custody is released) |
| entry custodies | the B escrow grows by exactly `⌈m·price/scale(A)⌉`, at its index, or a NEW escrow custody at the merge's `bOut` carrying `exitCarrier` when it held none | the A custody holds exactly `amountLeft + m` (new at `aOut` when it held none), the B prefund grows by exactly `⌈m·prefund/scale(A)⌉` (new at `bOut` when it held none); each new custody carries `exitCarrier` |
| entry continuation | same script, `amountLeft + m`, value ≥ in (− `exitCarrier` per new custody); on the exit's sell-out ≥ in + the exit's UTXO + the exit custody's carrier (input `xc`) − the exit's tip `⌊m·tip/scale(A)⌋` − its `deliveryCarrier` | same |
| exit continuation | custody `custody − m` of A | custody `custody − proceeds(m) − back(m)` of B |

The exit releases `⌈m·rptPrice/scale(A)⌉` (and `⌈m·rptPre/scale(A)⌉`) exactly as the entry requires them, so the two
covenants split one release with no remainder; the exit's custody input `xc` is authenticated by the entry like its own
custodies (genuine holder of the token's program, owned by the exit id, exactly the exit's `custody`).

**Sequencing rules for matchers.**
1. Before `rptUntil`, a booked exit's take-profit is only possible with its merge. After it (the
   entry expired, or was left idle for 90 days), the take-profit is plain.
2. A stop-loss (stop leg) fill of a booked exit is always plain and MUST NOT include its entry:
   the stopped-out amount leaves the cycle.
3. One entry merges one exit per transaction; two booked exits of the same entry take profit in
   separate transactions (they can be chained).
4. A merge transaction has no other spend of the entry (no fill, update or refund).
5. An exit's `update` (arm / trail) never runs in a transaction that spends its entry: the exit requires that no input
   carries its `parent` id. Signature-script pushes need not be minimal under covenants, so an `update(ev, …)` whose
   first push is 8 bytes could otherwise be read as a merge amount by the entry.
6. The entry needs `deliveryCarrier + exitCarrier` of its reserve per fill, returned when that exit
   sells out on its take-profit. With the default funding (§10.13) at most ⌈N/`minFill`⌉ exits are
   live at a time; an amount re-armed beyond that waits until an exit sells out. (Over the entry's life the number of
   fills is not bounded by ⌈N/`minFill`⌉: after every merge the "takes everything left" exception applies again, so a
   re-armed amount below `minFill` fills, and makes an exit, on its own.)
7. Merges are paid by the take-profit's crossing spread and the exit's `tip` (a merge adds
   the entry input, and sell-first its custody: buy-first 3,960 bytes and 0.0079 KAS, sell-first 8,270 bytes and
   0.0165 KAS over a plain take-profit; golden vectors `rpt.bid.merge.partial` vs `cond.ask.takeProfit.partial` and
   `rpt.ask.merge.partial` vs `cond.bid.limit.partial`, §9; a pair merge adds the entry and its custodies: buy-first
   11,862 bytes and 0.0237 KAS, sell-first into new custodies 8,428 bytes and 0.0169 KAS, `pair.rearm.bid` vs
   `pair.cond.ask.tp` and `pair.rearm.ask.new` vs `pair.cond.bid.limit`). Wallets SHOULD raise a repeat exit's tip
   accordingly.
8. The indexer derives every step: exits from the fill (§1.1), the entry's continuation after a
   merge from the merge argument, and, sell-first, the entry's new custody from the merge output.

---

## 7. Chaining, splitting, fees and RBF

- **Splitting.** When the crossing book set is larger than one transaction (the physical limits of §3.2), the matcher
  plans the next transaction on the book the previous one leaves, greedily: the first transaction takes the most
  profitable part (§3.1), each later one the most profitable part of what is left, until nothing profitable crosses or
  the tick's wall-clock budget runs out. Orders the earlier transactions did not touch are planned as they are; the
  continuations they created (partial fills) are unaccepted and only chain-safe fills of them are planned (no fill whose
  covenant path reads the parent's DAA score: no TWAP / DCA slice, stop auction, trigger or trigger evidence (a pair
  trigger's KAS-book or pair-order evidence included) or merge; the pair kinds follow the rule of their KAS
  counterparts).
  An order takes part in at most `max_chain` (default 4) transactions of one tick; with chaining off
  (`--no-chain`) a continuation waits for the next tick while untouched orders still go into the tick's next
  transactions.
- Each transaction of a tick is funded by the operator's coins in a deterministic order (largest first, the change of the
  tick's earlier transactions last; a P2PK spend never reads its parent's DAA score): the largest first, then as many more
  as the builder's shortfall needs (at most 8 by default), and, while the operator's pool is fragmented, up to two of the
  smallest merged into the change when the batch stays profitable with them (`docs/ops/executor.md`, funding). A
  transaction that spends an output of an earlier one of the tick names the latest such transaction as its parent and is
  submitted only after it.
- Continuations (partial fills, updates, exits) are spent in follow-up transactions; a matcher
  MAY chain unconfirmed transactions. Atomicity holds per transaction, not across a chain.
- Fee = `rate × max(compute, 2·bytes)` sompi with a two-pass compute budget; fee changes go to the
  matcher's own funding input, never to a covenant output. The fee of a batch is one fee for all its books.
- **Fee rate.** At least the relay floor (100 sompi per gram). A matcher SHOULD price by the node's fee estimate
  (`getFeeEstimate`: a priority bucket for sub-second inclusion, normal buckets for sub-minute, low buckets for sub-hour)
  and by urgency: a batch that fills an immediate order (IOC, FOK, market, streaming) or a triggered stop at the priority
  rate (the order's kill is a minute away, §5), other batches at the first normal bucket, the operator's own housekeeping at
  the first low bucket; kills at the priority rate, refunds at the normal one; within an operator cap per gram and per
  transaction, and the floor when the node gives no estimate (the reference executor: `docs/ops/executor.md`, "Fee
  policy"). The rate never changes what a covenant pays a maker: fees come out of the matcher's own funding (and a keeper's
  `refundTip`, a swap intent's own funds).
- **Profit at the rate actually paid.** A batch is planned at the normal rate and built only when its exact profit (the
  built transaction's `change − funding`) at the rate it is built at is at least the operator's minimum; an urgent batch
  that does not stay profitable at the priority rate goes at the highest rate that keeps it profitable, never below the
  rate it was planned at. A keeper job likewise pays at most what its tip leaves.
- Compute budgets come from the committed table (`kob_protocol::budget`, generated in the engine over every shape the
  builders accept, the largest batch contexts and every multi-cross route included: §9, "Script units"); a matcher that
  still meets `ExceededCommittedScriptUnits` measures the inputs and commits what they use (the sighash does not commit to
  budgets) rather than skipping the batch.
- The planner sizes a batch by an estimate of its bytes; when the built transaction exceeds a block mass limit, the
  budget is scaled down by the overrun and the batch planned again (no fill is dropped for it).
- Competing matchers race by fee; the loser's transaction is invalid, not harmful. The node offers replacement by fee
  (`submitTransactionReplacement`, RBF), but the reference executor never uses it: no transaction, its own or a
  competitor's, is replaced; a transaction stuck at a low rate stays pending until it is accepted or the pending timeout
  drops it (§8), and the next plan is priced at the rates of that moment. Re-submitting a stuck transaction at a higher fee
  is a possible later addition, not part of this specification.
- A cancel may lose to a fill and a fill may lose to a cancel (accepted spec).

---

## 8. Robustness checklist

- Never include a stray or a second UTXO of the same order.
- Never exceed 8 token inputs of one token per transaction (KRON 4), nor any token's program slots.
- Never fill an order below its `minFill` unless the fill takes everything left (a `KobBid`: unless it terminates).
- Never fill a booked exit's take-profit before its `rptUntil` without its entry's merge, never
  put an entry next to its exit's stop-leg fill or its exit's `update`, and never merge two exits into one entry in one
  transaction (§6.1).
- Never fill a pair order off its exact amounts: an ask releases exactly n of A and receives at least its ceil of B, a
  `KobPair` bid releases exactly its floor of B and receives exactly n of A; keep every custody exact and every
  positional output (its input index, its custody's index) for that order alone (§3.5).
- Never record a price, candle or last price from a pair fill; a pair fill whose counterparty is not a KAS-book order is
  volume only (§3.5).
- Never arm or trail a stop without its evidence filled in the same transaction, and never offer a
  conditional, if-done, pair or decaying fill, or one exposed for less than `minRestDaa`, as evidence of a KAS-quoted
  stop (§4.2); a pair stop reads only two plain KAS-book fills of its two tokens or a resting `KobPair` fill of its pair
  (§4.7).
- KRON: keep the authorising covenant input of every KRON token input below index 128 (§2.2).
- Recompute every covenant's inequality before submitting (the covenants are the reference), with the covenants'
  rounding (`quoteOf`).
- Mark an order done only after acceptance plus N DAA; roll back on `removed`.
- Rate-limit per order; back off on `RejectDoubleSpendInMempool` (the order whose input the node names, when it names one).

---

## 9. Measured costs (protocol v3; 10-KAS carriers, reference 3/3 token unless noted)

The fixtures carry 10 KAS on every covenant UTXO; the wallet default is 2 KAS (`DEFAULT_ORDER_CARRIER`). The relay fees
below do not depend on it (storage mass is not part of the relay fee): at 2 KAS no shape pays more, a few placements and
amends up to 200 sompi less (`crates/kob-protocol/tests/carrier_fees.rs`).

Rows are the golden vectors named in the row (`crates/kob-protocol/vectors/golden.json`: bytes = `built.fee.mass.size`,
fee floor = `built.fee.minFee`), the 8/8 cancel-replace is `crates/kob-protocol/tests/cost_table.rs` (KCC20Ref_8x8), and
the global batch rows are `crates/kob-executor/tests/matcher_batch.rs`. A fraction such as 4/10 is the part of an order's
amount a fill takes. The pair rows use the reference token programs of the vector's prefix (`pair.`: KCC-20 A and B;
`pair.kcc20-kron.`, `pair.kron-kcc20.`, `pair.kron-kron.`: the families of A and B).

| Shape | Bytes | Fee floor |
|---|---|---|
| taker buys 4/10 from one ask (S1; `take.ask.partial`) | 5,663 | 0.0113 KAS |
| market sell auction fill, IOC 4/10 (`take.ask.market`) | 5,579 | 0.0112 KAS |
| matcher crosses 1 bid × 2 asks (S2; `match.cross.1x2`) | 12,049 | 0.0241 KAS |
| B1 batch, 3 bids × 5 asks (8/8) + one stop armed by `update` next to an ask fill (`match.batch.3x5.8x8.arm`) | 52,772 | 0.1055 KAS |
| stop sell 4/10 armed inside its fill, with the evidence ask fill 1/5 (`cond.ask.stop.trigger`) | 12,712 | 0.0254 KAS |
| stop auction fill, armed (S17; `cond.ask.stop.auction`) | 7,515 | 0.0150 KAS |
| arm a stop sell by `update` next to a 1/5 ask fill, whole transaction (`cond.ask.update.arm`) | 9,298 | 0.0186 KAS |
| arm or trail (`update`), marginal inside a batch, largest over the updatable kinds (`data/keeper_tips.json` `updateFee`) | — | KAS-quoted kinds 0.0101 KAS (KRON 0.0091 KAS); the pair conditionals and entries 0.0178 KAS (either family) |
| IOC kill / refund of an ask (`refund.ask.iocKill`) | 5,159 | 0.0103 KAS |
| in-place amend of an ask, the carrier pays (any KCC-20 program / KRON; `amend.ask`), payload version 4 | 2,090 / 1,939 | 0.0042 / 0.0039 KAS |
| cancel-replace of an ask, 3/3 / 8/8 (`cancelReplace.ask`; no tiny change output, payload version 4) | 5,474 / 9,204 | 0.0109 / 0.0184 KAS |
| IFO entry partial 4/10 (S14; `ifd.bid.partial`) / a repeating entry (RP1; `ifd.bid.repeat.book`) | 10,766 / 10,769 | 0.0215 / 0.0215 KAS |
| stop-entry IFO 4/10 armed inside the fill, with the evidence bid fill (S18; `ifd.bid.stopEntry.trigger`) | 12,636 | 0.0253 KAS |
| sell-first IFO entry partial 4/10 (B7; `ifd.ask.partial`) / a repeating entry (RPB1; `ifd.ask.repeat.book`) | 11,822 / 11,825 | 0.0236 / 0.0237 KAS |
| repeat, buy-first: take-profit 3/4 + merge (RP4; `rpt.bid.merge.partial`) / sell-out 4/4 + merge (RP5; `rpt.bid.merge.sellOut`) | 11,472 / 11,192 | 0.0229 / 0.0224 KAS |
| repeat, sell-first: buy-back 3/4 + merge into custody (RPB4; `rpt.ask.merge.partial`) / 4/4 into a new custody (RPB5; `rpt.ask.merge.newCustody`) | 15,232 / 11,992 | 0.0305 / 0.0240 KAS |
| booked exit take-profit after `rptUntil`, no merge (RP6 / RPB6; `rpt.bid.takeProfit.afterUntil` / `rpt.ask.takeProfit.afterUntil`) | 7,232 / 6,875 | 0.0145 / 0.0138 KAS |
| close of an empty repeating ask (RPB8; `close.ifdAsk.emptyRepeat`) / refund of an if-done bid entry (RP9; `refund.ifdBid`: a non-repeating entry, the same template and entry as an empty repeating one) | 5,108 / 4,004 | 0.0102 / 0.0080 KAS |
| pair ask fill routed through 1 bid of A + 1 ask of B, rest (`pair.ask.rest`; `pair.kcc20-kron.ask.rest` / `pair.kron-kcc20.ask.rest` / `pair.kron-kron.ask.rest`) | 13,959 / 13,016 / 12,995 / 12,351 | 0.0279 / 0.0260 / 0.0260 / 0.0247 KAS |
| pair bid fill routed through 1 bid of B + 1 ask of A, rest (`pair.bid.rest`, same family mixes) | 13,959 / 12,995 / 13,016 / 12,351 | 0.0279 / 0.0260 / 0.0260 / 0.0247 KAS |
| pair market order mid-auction (`pair.ask.decay` / `pair.bid.rising`) | 13,962 / 13,962 | 0.0279 / 0.0279 KAS |
| two opposite pair orders netted, no KAS-book leg (`pair.net.1x1`) / 2 × 2 netted, KRON / KRON (`pair.kron-kron.net.2x2`) | 13,506 / 24,524 | 0.0270 / 0.0490 KAS |
| pair stop armed inside its fill, evidence mode 0 (two KAS-book fills) / mode 1 (a resting `KobPair` fill) (`pair.cond.ask.stop.ev0` / `pair.cond.ask.stop.ev1`) | 24,356 / 23,450 | 0.0487 / 0.0469 KAS |
| arm a pair stop by `update`, mode 0 / mode 1, whole transaction (`pair.update.ask.arm.ev0` / `pair.update.ask.arm.ev1`) | 20,807 / 19,901 | 0.0416 / 0.0398 KAS |
| pair if-done entry fill, buy-first / sell-first (`pair.ifd.bid.cont` / `pair.ifd.ask.cont`) | 25,574 / 28,923 | 0.0511 / 0.0578 KAS |
| pair repeat: take-profit + merge, buy-first / sell-first into new custodies (`pair.rearm.bid` / `pair.rearm.ask.new`) | 29,146 / 25,908 | 0.0583 / 0.0518 KAS |
| create / cancel / refund a pair ask (`pair.create.ask` / `pair.cancel.ask` / `pair.refund.ask`); create a sell-first pair entry (`pair.create.ifdAsk`, two custodies) | 4,244 / 6,654 / 6,602; 8,171 | 0.0085 / 0.0133 / 0.0132; 0.0163 KAS |
| global batch: two 8/8 books (2 bids × 2 asks; an IOC ask into two bids) and a routed pair order 8/8 / 8/8, one transaction | to be measured (pair phase) | to be measured (pair phase) |
| global batch at the physical limit: fills of 12 books of 8/8 tokens, the first of several transactions over 48 books | to be measured (pair phase) | to be measured (pair phase) |

**Script units of pair inputs.** A pair input reads, besides its own custodies and the template of the token it buys,
only its trigger evidence (a pair conditional or entry: one or two evidence state spans, and the custody input of an ask
or pair evidence) and, for a repeat merge, the exit's state; it scans every input of both of its tokens for strays (at
most 8 each, unrolled). It reads no counterparty: its cost does not depend on the route or the netting it is in. The
budgets are measured per shape and program pair by `crates/kob-protocol/tests/budget_table.rs` (pair roles
`<Kind>.<entry>…@<program of A>+<program of B>`, `data/compute_budgets.json`, units of 10,000 script units): on KCC20Ref
for both tokens `KobPair` 6 to 11, `KobCondPair` 7 to 21 (a stop armed in its fill 21, an update 11), `KobIfdPair` 7 to 34
(a sell-first fill that arms its stop entry and books its exit 34); over every program pair at most 72, 91 and 136 (the
largest token programs, whose custody inputs the scans read).

**Template sizes.** `KobPair` 2,919 B (state 414 B), `KobCondPair` 6,225 B (state 510 B), `KobIfdPair` 8,708 B (state
909 B; the committed exit 432 B). The scans of both tokens are unrolled to `MAX_TOK_IN` = 8 slots each (a KRON program
accepts fewer), so every spend of a pair order pays for them; a larger route or net is split into several fills (§7).

**Batches without caps.** The planner has no size cap and no candidate cap; a batch is filled up to the physical
limit (§3.2, 250,000 bytes; the next transaction of the tick chained on its change). Planning one batch
(`crates/kob-executor/tests/matcher_batch.rs`, debug build with `opt-level = 1`, KAS books): 1,000 bids × 1,000 asks
of one token 4.8 ms; 10,000 × 10,000 of one token 60.6 ms (the 8/8 slots bound the batch to 12 fills); ten tokens of
1,000 × 1,000 each 142.7 ms (36 fills, bounded by size); 10,000 × 10,000 all crossing at a loss 74.6 ms (nothing
built). A whole tick over the 10,000 × 10,000 book (plan, build, sign and engine-validate every transaction) runs to its
20 s budget and prepares 270 chained transactions (3,240 fills; the first 76,583 bytes); the rest waits for the next
tick. With pair books: to be measured (pair phase).

---

## 10. Wallet and client rules (normative for KOB wallets)

1. **Placement record**: publish §1.1 for every order; never send tokens to an order's covenant
   id (a top-up is a cancel-replace); fund asks with exactly `amountLeft` base units, a pair order's custodies with
   exactly the amounts its state names (§14). Every carrier that
   becomes a token output (custody, delivery, a sell-first exit's new custody) carries at least its token
   program's floor (KaspaCom KCC20 0.2.5 refuses token outputs below 0.5 KAS) and every output at least
   the KIP-9 dust bound of 0.02 KAS (a smaller output's storage mass alone exceeds the block limit); the
   builders refuse a transaction that breaks either ("dust payout").
2. **Market**: an IOC auction: `price` = the touch (best opposite all-in quote, sompi per whole token),
   `priceEnd` = the touch ∓ slippage bound (default 3%), `decayStep = 1`, slope (sompi per whole token per DAA) so that
   the bound is reached after the auction length (default 200 DAA = 20 s), `activeFrom` = current DAA + 30,
   `expiryDaa = activeFrom + 300`, `minFill` 1 base unit. Show "worst price" = the bound and "expected" = the touch.
3. **Streaming / quote-and-execute**: as market, starting at the displayed price with the user's
   slippage tolerance; FOK when the user asked for all-or-nothing.
4. **FOK / IOC**: default life 300 DAA (30 s; the covenant kills at 600); `minFill` 1 base unit. Reject a FOK at
   placement when the visible book cannot fill it within one transaction: more counterparties than the
   token's slot limit allows (FOK ask: bids ≤ token output slots; FOK bid: asks ≤ token input
   slots), more than 8 token inputs (KRON 4), or not enough crossing quantity at the counterparties' minimum fills.
5. **Marketable limit**: when a limit crosses the book at placement, place it as an auction from
   the touch to the limit (then resting at the limit), or warn that a crossing limit fills at its
   limit (the matcher keeps the difference).
6. **Stop-market / stop-limit**: `slipBps` default 300 (3%),
   `bandDaa` default 300 (30 s auction), `keeperTip` default: to be measured (pair phase) (2× the
   marginal fee of an update inside a batch, rounded up to 0.001 KAS; `kob_protocol::defaults`,
   `data/keeper_tips.json`). Trigger rules (§4) are
   per order; the wallet MUST expose both on every stop, stop-limit, trailing stop, OCO stop leg and stop entry:
   - rest time `minRestDaa`: default 50 DAA (5 s);
   - threshold `minTouch` (base units): chosen by the user; default the order's own `minFill`; the ticket offers min
     fill / 25% / 50% / 100% of the order's own amount and a custom amount (§4.3), and the confirmation screen shows
     the chosen threshold as a token amount.

   A stop-limit's limit L is converted to the largest `slipBps` with `stop ∓ ⌊stop·slipBps/10⁴⌋` not beyond L, i.e.
   `slipBps = min(10000, ⌊((|L − stop| + 1)·10⁴ − 1) / stop⌋)`. Disclose: "a sell stop triggers
   when a sell order at or below your stop has rested ≥ 5 s and at least your threshold of it is
   bought in one transaction (a buy stop: a buy order at or above your stop, sold into); unlike a
   broker's stop, sells hitting resting bids do not trigger it, so in a fast fall it can trigger
   later; a trade that no matcher uses to arm your stop does not count, the next one does; then
   sells at the best price found within the band". A pair stop discloses its rule (§4.7): "arms when the two KAS books
   imply a rate beyond your stop (a sell stop: a resting sell of A and a resting buy of B; a buy stop: a resting buy of A
   and a resting sell of B; each rested ≥ 5 s and filled together, at least your threshold of A and its value in B), or
   when a resting pair order at or beyond your stop is filled (at least your threshold)".
7. **Trailing**: disclose the step, the gap and the rate (≤ one update per `trailWait`);
   fund `keeperTip` per expected update from the carrier.
8. **IFD / IFO / bracket**: `minFill` default `⌈amount / 4⌉`; fund `⌈amount / minFill⌉ × (deliveryCarrier
   + exitCarrier)` plus the budget (buy-first: `⌈amount·(price + tip)/scale⌉`; sell-first: the tokens and
   `⌈amount·prefund/scale⌉`); exits default to GTC (90-day idle from each exit's creation);
   show the entry and its exits as one position. A pair entry (`KobIfdPair`) also funds the tip of its whole amount in
   KAS, and its B custody: buy-first the escrow `⌊amount·price/scale(A)⌋`; sell-first the tokens (exactly
   `amountLeft` of A) and the prefund `⌈amount·prefund/scale(A)⌉` plus one base unit per possible fill but the last
   (`IfdPairState::b_custody_needed`), with `price + prefund` covering the exit's worst buy-back per whole A.
9. **Cancel / cancel-replace**: one transaction, SIGHASH_ALL; re-read the current
   continuation first; an amended armed stop starts unarmed; offer "cancel all" of a position's
   exits in one transaction; sweep strays with the cancel, or IN PLACE before (a `SWEEP`, the order lives on). Amend a
   plain ask IN PLACE (`AmendOrder`, an `AMEND` record) when the new terms keep its `scale` and `amountLeft` and it has
   no strays to sweep: its custody stays, the order keeps its id, and its carrier pays the fee; a plain bid likewise
   when it keeps its token and `scale` (its escrow pays the fee, or maker funding tops it up); a pair order is never
   amended in place (cancel-replace; a `SWEEP` works for it as for every kind); never continue a
   cancelled order's id without a valid `AMEND` record (indexers list such a continuation nowhere). Keep the maker's
   local order record in step (its state is the amended one). No tiny change output: a change below
   `10^12 / feeMass` sompi rides on the new order (`kob1-payload.md`, *Change*).
10. **Expiry**: DAA ↔ wall clock from the node's virtual DAA score `D0` and the wall clock `T0`
   (UTC, NTP-synced) read together at signing, at the rate `r` = DAA advance per second measured
   by the node over the last hour (default and fallback 10 DAA/s; clamp to [9.5, 10.5]); show
   "expires around". GTC = 90 days idle; remind the user to renew (cancel-replace) before day 85.
   **Day order = until the next 00:00 UTC.** With `Δ` = seconds from `T0` to that midnight:
   - `expiryDaa = D0 + ⌈Δ·r⌉ + ⌈0.01·Δ·r⌉` (the estimate plus a 1% margin), so the on-chain
     refund opens at or after midnight unless the DAA rate over the order's life beats `r` by
     more than 1%;
   - the placement record carries `deadline` = that midnight (§1.1); conforming matchers stop
     filling at `deadline` by their own UTC clock (§2.3), so the user-visible cut-off is 00:00 UTC
     regardless of DAA drift;
   - tolerance: between `deadline` and the on-chain refund (≤ 1% of `Δ` plus the rate error, at
     most ≈ 15 min for an order placed just after midnight, seconds for an order placed late in the
     day) the order is closed for conforming matchers but not yet refundable; a non-conforming
     fill in that window still respects the limit (soft expiry, accepted spec). The wallet shows
     "day order: until 00:00 UTC (09:00 JST)" and refunds itself at `expiryDaa` when online.
11. **Close**: spot closing = a market sell of the held amount (or cancel the exits
   and sell); "close all" sells the whole balance of the token.
12. **Self-trade**: never place an order that crosses the user's own resting order.
13. **Repeat IFD / IFO**, §6.1: `rptAmount = 1 + K·N` for K repeats of N base units
   (K = 0: an IFD that never re-arms; "unlimited" = the 90-day bound, K large; `N < 2^53`);
   `expiryDaa ≤` placement + 77,760,000 DAA (90 days); fund the entry with the IFD budget plus
   ⌈N/`minFill`⌉ × (`deliveryCarrier` + `exitCarrier`) plus one more `exitCarrier` (its own carrier:
   a repeating entry outlives its last fill); require the take-profit to beat the entry per whole token (buy-first:
   `tp − tip > price + tip`; sell-first: `price − tip > tp + tip`; sell-first also `price ≥ tip`, which the covenant
   requires) and, sell-first, `prefund` to cover the stop leg's worst buy-back; add the merge cost to the exits' `tip`
   (§6.1 rule 7); a pair entry (`KobIfdPair`) likewise, in B per whole A without the tip (buy-first `tp > price`,
   sell-first `tp < price`). Disclose: "re-buys (re-sells) the same quantity at the same price after every take-profit, up to K
   times or until the order expires; a stop-loss ends the repeat for that amount". Cancelling a repeat cancels the
   entry and all its exits in one transaction (booked exits cannot take profit without their entry until their
   `rptUntil`).
14. **Pair orders** (§3.5; `order-types.md`, *Pair orders*): the pair ticket offers exactly the order types of the KAS
   ticket, on `KobPair`, `KobCondPair` and `KobIfdPair`; A ≠ B. Amounts are token amounts of A, traded exactly as typed
   (no rounding of the size); prices are in B per whole A, converted to base units of B per whole A rounded in the
   maker's favour (an ask UP, a bid DOWN), with `scale` of each token `10^decimals`, capped at `10^9`. Show the price per
   whole A and the guaranteed totals (an ask receives at least `⌈amount·price/scale(A)⌉` of B, a bid pays at most
   `⌊amount·price/scale(A)⌋`). Fund an ask's custody with exactly `amountLeft` of A; a `KobPair` bid's B escrow with
   `⌈amount·pMax/scale(A)⌉` plus one base unit per possible fill (`PairState::bid_escrow`), a `KobCondPair` bid's with
   `⌊amount·worst/scale(A)⌋` plus one per possible fill (`CondPairState::bid_escrow`; the escrow left returns with the last
   fill); if-done entries as item 8. The order UTXO prefunds `deliveryCarrier` (at least a token output's floor of the
   token it delivers) for each of the `⌈amount / minFill⌉` possible fills and the KAS tip of the whole amount,
   `⌊amount·tip/scale(A)⌋` (an entry also `exitCarrier` per fill; unused KAS returns with the last fill, the refund or the
   cancel); `refundTip` by the programs of its custodies (§5); `minFill` as in item 16 (the amount of A worth 10 KAS on
   A's KAS book); the trigger disclosure of item 6. A market order on a pair is a `KobPair` IOC auction like item 2: `price`
   = the touch (for the order's side, the better of the best opposite pair order and the route level of the two KAS books,
   in base units of B per whole A, rounded in the maker's favour), `priceEnd` = the touch ∓ the slippage bound (default
   3%), `decayStep = 1`, `slope` so that the bound is reached after 200 DAA (20 s), `activeFrom` = current DAA + 30,
   `expiryDaa = activeFrom + 300`, `minFill` 1 base unit, a bid's escrow sized at `priceEnd`. Show "fills between
   {touch} and {worst}; better if matchers compete". Disclose that a pair order may be filled through the KAS books,
   against other pair orders or from a matcher's own tokens, always at least at its own terms, and that a fill against
   another pair order shows as volume, not as a price.
15. **Network fee**: price every transaction by the node's fee estimate and its urgency, as §7 does for matchers: IOC /
   FOK and market-like placements (a keeper kills them a minute after they rest) and x402 payments (a merchant waits) at
   the priority bucket; resting placements, amends, cancels and issuance at the first normal bucket; UTXO merges,
   fan-outs and other housekeeping at the first low bucket; at least the relay floor (100 sompi per gram), within a cap per
   gram and per transaction the user can see, and the floor when the node gives no estimate. Before signing, show the fee
   in KAS, the rate, its urgency and the estimated time, and say so plainly when the estimate was unavailable (the floor
   may confirm slowly on a busy network) or the cap lowered the rate.
16. **Amounts, prices and the minimum fill** (`order-types.md`, *Amounts, prices and rounding*): enter and show amounts
   as token amounts, store them in base units; `scale` = `10^decimals` of the base token, capped at `10^9`; prices per
   whole token (sompi, or base units of B per whole A for a pair order); refuse an order outside the numeric gate (`scale ≤ 10^9`, a power of ten, `amount·rate/scale < 2^62`
   for every rate it carries). `minFill` default: the amount worth 10 KAS (`DEFAULT_MIN_FILL_SOMPI`, a notional independent of the carrier) at the limit price,
   at least 1 base unit and at most the order's amount; IOC, FOK and market orders 1 base unit; if-done entries item 8.
   Show the minimum fill on the ticket and the confirmation screen.

---

## 11. Conformance tests the executor must pass

- FOK never partially included; IOC maximal; oversize FOK rejected by the wallet.
- IOC / FOK refunded within one block after the kill time.
- Auctions filled at the first profitable t.
- Every fill at least the order's `minFill` unless it takes everything left (a `KobBid`: unless it terminates); every
  amount the covenants check recomputed with their rounding (`quoteOf`).
- Touch trigger (§4): stops, trailing stops and stop entries armed or ratcheted only in a batch that
  fills qualifying evidence (arm-in-fill or `update` next to the evidence fill); never from a
  conditional, if-done, pair or decaying fill (a pair stop: §4.7), from evidence exposed for less than
  `minRestDaa` before the lock time, from evidence of another `scale`, or below `minTouch`; the side rule of §4.1; one
  fill arming several stops; a batch that fills qualifying evidence arms every listed stop it
  qualifies when the income covers the cost (§4.4); an armed stop's auction filled from the
  next transaction.
- Trailing updates with the maximal justified k, at most one per `trailWait`.
- If-done exits built with the exact genesis id; exits visible to the indexer without a new
  placement record.
- Strays flagged and never spent; batches never exceed 8 token inputs of one token (KRON 4).
- Reorg rollback of fills, updates and exits.
- Repeat IFD: every take-profit of a booked exit before its `rptUntil` carries its entry's merge
  with the exact merge argument `−(k·2^53 + m)`; stop-loss fills and exit updates never carry the entry; a sell-first
  merge builds the entry's custody with exactly `amountLeft + m` base units (a new custody when it had none); the
  indexer follows the entry through fills and merges without new placement records.
- Day orders are not filled after their `deadline` (UTC) even while `expiryDaa` is ahead.
- Pair orders (§3.5): every fill at the order's exact amounts (an ask: exactly n of A out, at least `⌈n·p/scale(A)⌉`
  of B to the maker; a `KobPair` bid: exactly `⌊n·p/scale(A)⌋` of B out, exactly n of A to the maker), the KAS tip
  `⌊n·tip/scale(A)⌋`, custodies exact, positional outputs and strays of both tokens respected, FOK all-or-none, IOC
  maximal; opposite pair orders of a pair netted before routing (any number per side), remainders routed through the
  KAS books and ranked with the direct orders by price → tip → age at their implied quotes; no inventory in the
  reference planner unless its opt-in surplus-inventory policy keeps an allowlisted surplus (the pair ask then gets
  exactly its floor); within 8 inputs of each token (KRON programs fewer) and both programs' slots, KRON authorising
  inputs below 128; the pair book lists the pair orders and the route levels.
- Pair triggers (§4.7): pair stops, trailing stops and stop entries armed or ratcheted only next to their evidence in the
  same transaction, in mode 0 (a plain ask of A and bid of B, or a bid of A and ask of B, each rested `minRestDaa`, not
  decaying, at the tokens' scales, `n_A ≥ minTouch` and `n_B ≥ ⌈minTouch·stop/scale(A)⌉`, the exact implied-rate
  comparison) or mode 1 (a resting `KobPair` of the same pair and scales, the side rule); a pair fill never arms a
  KAS-quoted stop.
- Price record (§3.5): prices, candles and the last price only from KAS-book fills; a netted or inventory pair fill is
  volume only and flagged; pair charts derived from the two KAS series.
- Global batch (§3): one transaction fills several token books and pair orders, each token within its own slots;
  no size or candidate cap below the physical limits; a book set larger than one transaction is split, most profitable
  first; the plan is a pure function of the view, `t` and the configuration.
