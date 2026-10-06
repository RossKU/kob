# Kaspa x402 exact: swap-and-pay extension (`extra.route`)

Status: Draft proposal. Addressed to [elldeeone/kaspa-x402](https://github.com/elldeeone/kaspa-x402)
(v1.0.0-rc.1, binding `kaspa-exact-v2`, checked against commit `040b1ec`). Optional extension of
the `exact` scheme; it does not change requirements or payloads that do not carry `extra.route`.
Depends on the companion proposal `x402-kcc20-profile.md` (the `kcc20` profile, the payload
commitment `kob-x402-payload-commitment-v1`, covenant fields in the safe-json projection, custody
classes). Reference implementation: KOB, crate `kob-x402` (`wire.rs`, `common.rs`, `swap.rs`,
`client/swap.rs`), which is the source of the field names below.

The key words MUST, MUST NOT, REQUIRED, SHOULD, SHOULD NOT and MAY are used as in RFC 2119.

## 1. What it is

Swap-and-pay lets a payer who holds token A (or KAS, when the merchant wants token B) pay a merchant
that asks for KAS or for token B, in one Kaspa transaction, through named resting orders of an
on-chain order book. Token A may be of either token family KOB trades (a KCC-20 program of the
pinned list, KaspaCom's third-party template included, or a KRON token); token B is a KCC-20 token.
The payer's token A is sold into one or more orders, the proceeds and any further order legs are
routed to the merchant in the same transaction, and either the whole transaction is accepted or none
of it is. The merchant never holds A and never touches the book; it receives exactly the advertised
amount of KAS (profile `standard-native`) or of token B (profile `kcc20`).

```text
payer holds A --sell into bid(s) of A--> KAS --(pay merchant)------------------> merchant gets KAS
                                          \--buy from ask(s) of B---------------> merchant gets B
                                     all in ONE transaction, atomic
```

The proposal has two modes and two layers. In the **payer-signed** mode (`kob-swap-v1`, sections 3 to 16)
the payer signs one transaction over named orders; in the **intent** mode (`kob-intent-v1`, section 17) the
payer signs once a transaction that locks its funds in an intent, and the facilitator executes the intent
against the book itself, choosing again when an order is taken. Section 18 adds invoices: the requirements
of one sale served at a URL, paid once, with an invoice-level status. The layers:

- an **abstract extension** (sections 3 to 5, 7 to 9): a requirement `extra.route`, a payload
  `route`, and verifier rules that hold for any order book whose orders satisfy the abstract
  properties of section 4 ("named-order positional outputs");
- a **KOB instantiation** (section 6): the order programs `KobBid` and `KobAsk`. Everything KOB
  specific (order templates, the KOB1 payload, the KOB indexer) is an informative reference, so
  another order-book implementation can implement the same extension.

## 2. Motivation

- A payer holding a token no merchant asked for should not have to swap first in a separate
  transaction (two settlements, an intermediate balance, a price move in between). One transaction
  removes the intermediate state.
- The merchant's custody exposure is limited to what it receives. It never holds A, so the custody
  class of A (companion proposal, section 4) is not the merchant's risk; it receives KAS, or B with
  B's declared custody class.
- Orders are shared UTXOs that anyone may spend, so the design must say what a verifier does with
  outputs the orders themselves force (section 8), and what happens when another taker wins an order
  (section 10).

## 3. Requirements

An offer entry that accepts routed payments is an ordinary exact entry (companion proposal for
`kcc20`, the binding for `standard-native`) with one extra member:

```json
{
  "scheme": "exact",
  "network": "kaspa:testnet-10",
  "amount": "100000000",
  "asset": "KAS",
  "payTo": "kaspatest:...",
  "maxTimeoutSeconds": 60,
  "extra": {
    "binding": "kaspa-exact-v2",
    "profile": "standard-native",
    "finality": "accepted",
    "transactionEncoding": "kaspa-sdk-safe-json-v2.0.0",
    "payToScriptPublicKey": "0000...",
    "route": {
      "binding": "kob-swap-v1",
      "critical": true,
      "payAssets": [
        {
          "asset": "<covenant id of token A, 64 lowercase hex>",
          "templateHash": "<64 hex>",
          "extensionCommitment": "<64 hex>"
        }
      ]
    }
  }
}
```

- `asset`, `amount`, `payTo` and `extra.profile` describe what the **merchant receives**, not what
  the payer spends: for `standard-native`, `asset` is `KAS` and `amount` sompi; for `kcc20`, `asset`
  is B's covenant id, `amount` is B base units, and `extra.token` is exactly as in the companion
  proposal. A wallet MUST NOT present `amount` as the payer's cost (section 9).
- `extra.route.binding` MUST equal `kob-swap-v1`.
- `extra.route.critical` MUST be `true`. The binding says a verifier ignores unknown `extra` fields
  "unless the selected binding explicitly marks them as critical"; this proposal marks `extra.route`
  critical. A client that does not implement this extension MUST NOT select the entry, and MUST skip
  it in offer selection. (A plain direct payment against this entry would be rejected: the verifier
  dispatches on the presence of `route`.)
- `extra.route.payAssets` MUST be a non-empty array (at most 16 entries) of distinct identities the
  server accepts as the payer's asset: `asset` (covenant id), `templateHash` and
  `extensionCommitment`, with the same meaning as in the companion proposal. The server MUST also
  hold every listed token in its token allowlist with matching pins. The entry `{ "asset": "KAS" }`
  (no pins) lets the payer pay in KAS; it is valid only when the merchant receives a token (profile
  `kcc20`). The token family is the family of the pinned program (`templateHash`),
  so the entry needs no family member: a KRON token is listed with its KRON program hash and an
  all-zero `extensionCommitment` (KRON has no extension field), a KaspaCom-template token like any
  other KCC-20 token. Only the merchant asset (`extra.token`, profile `kcc20`) is restricted:
  `extra.token.family` MUST be `kcc20`, so a KRON token can be paid with but never received.
- A server SHOULD emit a routed entry as a separate element of `accepts`, next to the direct
  entries, rather than folding routes into the direct entry.
- `extra.route` MUST be absent from `additive` entries.
- The whole `extra` object, including `route`, is inside `paymentRequirementsHash`, so a payer cannot
  change the accepted pay assets.

## 4. Abstract requirement: named-order positional outputs

This section is independent of KOB. An **order** is an unspent output locked by a covenant program
(the order program) such that, when it is spent by a transaction:

- **(O1) Terms enforced by script.** The program validates the order's own terms (price, quantity,
  custody) against the transaction's outputs. A verifier that runs the covenant scripts therefore
  knows the price is honoured without understanding it.
- **(O2) Positional counterparty output.** The program forces the counterparty side of the fill at
  the output whose index equals the index of the order's input. The order spent at input `i` has its
  payout or delivery at output `i`.
- **(O3) Identifiable residue.** Whatever else the order forces is identifiable from trusted data:
  a continuation carries the order's covenant id in its covenant binding; a custody remainder is a
  token output owned by the order's covenant id.

An implementation of this extension needs a **claim function**: given the transaction and the
trusted UTXO entries of its inputs (never payer hints), it returns for every output whether it is
**claimed by an order** (a positional slot `0..k-1`, a continuation, or a custody remainder) or
**unclaimed**. Positional slots are claimed by definition, whatever they contain. The extension's
verifier rules (section 8) consume only this function. An order book that cannot provide a claim
function, or whose orders do not satisfy O1 to O3, cannot implement the extension safely.

## 5. Payload

The payload is the `exact-transaction` payload of the companion proposal with:

- `profile`: the merchant-gain profile, `standard-native` or `kcc20`, equal to the offer's;
- `route`, required, and only when the offer carries `extra.route`:

```json
"route": {
  "binding": "kob-swap-v1",
  "payAsset": "<covenant id of token A, or KAS>",
  "orders": [ { "txid": "<64 hex>", "index": 0 }, { "txid": "<64 hex>", "index": 1 } ]
}
```

- `authorization`: the payload commitment (`kob-x402-payload-commitment-v1`), with the digest object
  of the companion proposal and `route = { "binding": "kob-swap-v1", "payAsset": "<covenant id>" }`
  inside it. The commitment is embedded in the transaction payload as a KOB1 `X402` record, so every
  input authorizer's SIGHASH_ALL covers it. `standard-native` entries that carry `route` use this
  authorization instead of the binding's Schnorr-signed one, because the payer inputs of a routed
  transaction include token inputs and the transaction includes order inputs that carry no payer
  signature.

`route.payAsset` MUST be one of the offer's `payAssets`. `route.orders` are **hints**: the verifier
derives the order inputs from the transaction and its trusted UTXOs and MUST NOT rely on the list. The
list has two uses: a facilitator can return the spent ones on conflict without parsing, and a
payer's client can show which orders it named. A verifier MAY reject a payload whose list disagrees
with the derived orders.

## 6. KOB instantiation (informative)

KOB's order book (`docs/spec/order-types.md`, `docs/spec/matcher.md`) provides the abstract orders
with two covenant programs per token family, both over one token and KAS. The tables and rules below
are stated for the KCC-20 family; the KRON family (`KobBidKron`, `KobAskKron`) is the same pair of
programs over the KRON token state (family table below):

| Program | Holds | Fill entry | Positional rule (O2) |
|---|---|---|---|
| `KobBid` (limit buy of token A) | KAS budget | `fill(n)`: anyone may fill n base units | the tokens bought by the order at input `i` are delivered at output `i`, as a KCC-20 UTXO owned by the maker (owner scheme `0x00`, borrow disabled) |
| `KobAsk` (limit sell of token B) | token custody plus a carrier | `settle(n > 0)`: anyone may fill n base units | the maker's KAS for the order at input `i` is paid at output `i`, to P2PK(maker) |

A partial fill continues the order at the same covenant id with the same script (a continuation
output with a covenant binding); an ask's custody remainder is a KCC-20 UTXO owned by the order's
covenant id (owner scheme `0x04`). Amounts are token base units and prices are sompi per WHOLE token
(`price` per `scale` base units, `scale = 10^decimals`, a state field of the order; `order-types.md`,
"Amounts, prices and rounding"). Prices are all-in limits enforced by the scripts, rounded in the maker's
favour: a bid pays at most `floor(n × (price + tip) / scale)` for n base units, an ask receives at least
`ceil(n × (price − tip) / scale)`; every fill respects the order's minimum fill `minFill` (n >= minFill
unless it takes all that is left, or for a bid unless it ends the bid). The
crossing spread belongs to whoever builds the transaction, which in a swap-and-pay transaction is the
payer (it stays in the payer's KAS change). Order programs are identified by their pinned template
hash (`KobBid`, `KobAsk`, `KobBidKron`, `KobAskKron`; see `kob1-payload.md`), and the order's
parameters are read from the state span of the redeem script in its input's signature script.

**Token families.** Every leg follows the family of its token: an order of a KCC-20 token is a
`KobBid` / `KobAsk`, an order of a KRON token a `KobBidKron` / `KobAskKron`, and the order's
token template hash names the program (the verifier checks that the order kind, the token program
and the allowlist entry agree on the family). The families differ only in the token state:

| | KCC-20 (any pinned program, KaspaCom template included) | KRON |
|---|---|---|
| state | 112 bytes, `extension_commitment` pinned | 46 bytes `owner id_type amount is_minter`, no extension |
| payer token (key-owned) | owner scheme `0x00` (P2PK), signed by the token leader | `id_type 3` (address presence): authorised by a P2PK input of the owner in the transaction, no signature of its own |
| ask custody / remainder | owner scheme `0x04`, borrow disabled | `id_type 2`, `is_minter 0` |
| "borrow disabled" (section 8.1) | `borrow_scheme 0`, `borrow_guard 0` | `is_minter 0` |

A route may cross families in one transaction (KRON sold into a `KobBidKron`, a KCC-20 token bought
from a `KobAsk`); each token keeps its own program, slot limits and signature-script columns. A
KaspaCom-template token is an ordinary KCC-20 token: it needs no special case, only its pinned
template hash in the allowlist (its 25.5 KB program is revealed in every token input's signature
script, so the size bounds of section 16 apply to it).

The extension has two leg kinds: a `KobBid` (the payer sells A into it; its KAS funds the route)
and a `KobAsk` (the payer buys B from it; B goes to the merchant). The reference verifier
classifies an order by its pinned template and does not inspect its pricing fields (the engine
enforces them); the reference quoter (section 12) plans only plain orders (GTC, constant price, no
TWAP or DCA interval, already active). A pinned order template of any other kind (conditional, stop,
if-done, cross) is rejected as `route_unsupported`; an unknown covenant as `unknown_order_template`.
Route shapes:

| Merchant receives | Payer pays | Legs | Example (KOB test scenario) |
|---|---|---|---|
| KAS (`standard-native`) | A (KCC-20 or KRON) | one or more bids of A | SW1 |
| B (`kcc20`) | KAS | one or more asks of B | SW2 |
| B (`kcc20`) | A (KCC-20 or KRON) | bids of A, then asks of B (B: KCC-20) | SW3, SW4 |

## 7. Transaction layout

A routed payment is a version-1 native transaction (zero gas, zero lock time, KOB1 payload with the
commitment):

```text
input[0..k)   order inputs, k >= 1 (bids, asks); the order at input i owns output slot i
input[k..)    order custody token inputs (ask legs: token UTXOs owned by an order's covenant id),
              payer token inputs of the pay asset A (key-owned; none when the payer pays KAS),
              payer P2PK KAS inputs (required when A is a KRON token: they authorise it)

output[0..k)  the positional slots of the orders, in input order (payouts and deliveries)
then, in any order:
              order continuations and custody remainders   (claimed by orders)
              the merchant output                          (at paymentOutputIndex, unclaimed)
              payer token change, payer KAS change         (unclaimed, payer-controlled)
```

Rules:

- Inputs `0..k-1` MUST be exactly the order inputs, and no order input may appear at index `k` or
  later: the positional rule ties the input index to the output index.
- Every token family in the transaction has its own leader (the input with the lowest index that
  carries the family's covenant id). Every output bound to a token family names that family's leader
  as `authorizingInput`. In a bid + ask route the leader of A is the payer's first A input and the
  leader of B is the first ask custody input; an order continuation's `authorizingInput` is the
  order's own input.
- `paymentOutputIndex` names the merchant output. It MUST NOT be a positional slot.
- All payer signatures use SIGHASH_ALL (no ANYONECANPAY, NONE or SINGLE). Order inputs carry no
  payer signature; they are satisfied by their covenant scripts. The verifier enforces the rule
  (section 8.1a).

Worked layout, KOB scenario SW3 (payer sells 4 whole tokens of A into a bid, buys 3 whole tokens of B
from an ask, merchant receives B):

| Index | Input | Output |
|---|---|---|
| 0 | `KobBid(A)` order | slot 0: A delivered to the bid's maker (claimed) |
| 1 | `KobAsk(B)` order | slot 1: KAS payout to the ask's maker (claimed) |
| 2 | payer A token UTXO (P2PK owner, leader of A) | bid continuation (claimed) |
| 3 | ask custody B token UTXO (owner = ask id, leader of B) | ask continuation (claimed) |
| 4 | | ask custody remainder of B (claimed) |
| 5 | | merchant B output (`paymentOutputIndex`, unclaimed) |
| 6 | | payer A change (unclaimed) |
| 7 | | payer KAS change (unclaimed) |

## 8. Verification

The verifier follows the binding's step list and the companion proposal's steps for the merchant
profile (envelope, re-derivation of `payToScriptPublicKey`, size bounds, canonical decode and id,
trusted UTXO resolution, hint comparison, signatures and witnesses, fee and mass, engine validation,
authorization, expiry, replay), with the profile's canonical-transaction and merchant-gain rules
replaced by the following.

### 8.1 Identify every input

Every input MUST be exactly one of:

1. an **order input** of a supported order program: the trusted UTXO script equals the P2SH of the
   redeem script in the input's signature script, and the redeem script's template hash equals a
   pinned order template (KOB: `KobBid`, `KobAsk` and their KRON twins). Its state is decoded from
   the redeem script. An unknown covenant or template MUST be rejected (`unknown_order_template`),
   never skipped (a pinned template of an unsupported order kind is `route_unsupported`);
2. an **order custody token input**: a token UTXO of an allowlisted token whose owner (KCC-20 scheme
   `0x04`, KRON `id_type 2`) is the covenant id of an order input of the same transaction;
3. a **payer token input** of `route.payAsset` (absent when it is `KAS`): a key-owned UTXO (KCC-20
   owner scheme `0x00`, KRON `id_type 3`) of an allowlisted token with the pinned program and
   extension commitment. A KRON token input requires a payer P2PK input of the same key in the transaction (the covenant checks
   it too); the order kind of every leg MUST be the kind of its token's family;
4. a **payer P2PK funding input**: a standard Schnorr P2PK KAS UTXO without a covenant.

Anything else is rejected. In particular, every token UTXO of the payer, of the merchant and of an
order's custody MUST have `borrow_scheme = 0x00` (a KRON token UTXO MUST have `is_minter = 0`); a
borrow-enabled UTXO is refused for the reason
given in the companion proposal (a third party can respend it between signing and broadcast, which
invalidates the payer's signed transaction).

### 8.1a Signature hash type

Before engine validation the verifier MUST read the hash type of every payer signature from its
signature script and refuse any hash type other than SIGHASH_ALL with `token_owner_scheme` (the
script engine accepts a correctly signed NONE, SINGLE or ANYONECANPAY transaction, so only this check
refuses it). The payer signatures are those of the payer P2PK funding inputs (one 65-byte push,
`sig64 ‖ type`) and of the payer's key-owned KCC-20 token inputs (the owner witness of the leader or
of a delegator, parsed per owner scheme as in `x402-kcc20-profile.md` step 8). A KRON token input
carries no signature of its own: its owner authorizes it with a payer P2PK input, which is checked
as a funding input. Order inputs and order custody inputs carry no payer signature. A malformed
signature script is `invalid_kaspa_exact_signature`. The same rule applies to the payer inputs of an
intent creation (section 17.5).

### 8.2 Claim the order outputs

Compute the claim function of section 4 from trusted data:

- outputs `0..k-1` are claimed (positional slots), whatever their script, value or state;
- every output with a covenant binding whose covenant id is the id of an order input is claimed
  (continuation);
- every token output owned by the covenant id of an order input (owner scheme `0x04`) is claimed
  (custody remainder).

### 8.3 Exact merchant gain over unclaimed outputs only (anti-aliasing)

The merchant gain is computed over **unclaimed outputs only**:

- `standard-native`: exactly one unclaimed output pays `payToScriptPublicKey`, and its value equals
  `amount`;
- `kcc20`: exactly one unclaimed output has script `extra.token.tokenScriptPublicKey`, value
  `extra.token.carrier`, and is bound to the covenant id of B (which fixes amount, owner and the
  borrow-disabled state), authorized by B's leader.

If the merchant's terms are matched by a claimed output but by no unclaimed output, the verifier
SHOULD report `route_aliasing`; if by no output at all, `underpayment`; if more than the required
gain is present, `overpayment`.

### 8.4 Strict allowlist for the other unclaimed outputs

Every unclaimed output other than the merchant output MUST be one of:

- **payer token change** of `route.payAsset`: an output bound to that token's covenant id, whose
  script is recomputed by the verifier from `state = { amount, owner = an owner key of one of the
  payer's token inputs, owner_scheme 0x00, borrow_scheme 0x00, borrow_guard 0, extension_commitment
  = pinned }` with the pinned program (KRON: `{ owner, id_type 3, amount, is_minter 0 }`);
- **payer KAS change**: a P2PK Schnorr output to the key of a verified payer input, without a
  covenant binding.

A verifier MAY additionally accept, for a `kcc20` route, a payer-owned output of the merchant token
holding the surplus of a purchase larger than the offered amount (the reference verifier does
not); builders SHOULD avoid the case by choosing amounts that need no surplus (any base-unit amount
that respects the orders' minimum fills is fillable). Any other unclaimed
output is rejected (`invalid_kaspa_exact_payment_output`). There is no "other outputs are fine"
path: the fee is the only place value may leave the payer's control.

### 8.5 Engine validation makes the order covenants enforce prices

The verifier MUST validate the whole transaction with the script engine under the enforced compute
budget, executing every input including every order covenant, and MUST check the recomputed masses,
committed storage mass and fee floor as in the binding. Because O1 holds, a transaction that passes
has each order's price, quantity and custody rules satisfied by the consensus rules themselves; the
verifier does not compute prices. A failure attributable to an order input whose outpoint is
unspent but whose covenant rejects the transaction is `order_not_spendable`. (Not implemented: the
reference verifier does not single this case out and reports the engine failure as
`invalid_kaspa_exact_transaction`; a spent or missing order is `order_conflict`, section 10.)

### 8.6 Worked example: the aliasing attack

Setup. The merchant M offers a routed `standard-native` entry: `amount = 100000000` sompi (1.00
KAS), `payTo = P2PK(M)`, `payAssets = [A]`. M also trades: it has an open `KobAsk` selling 1000
base units of token B (one whole token, `scale` 1000), all-in price 1.00 KAS per whole token, maker P2PK(M), so its ask pays P2PK(M) at the
ask's positional slot. Q has a resting `KobBid` for A. The payer P holds 1000 A.

Honest payment H: P sells A into Q's bid, and pays M from the proceeds.

| | Inputs | Outputs |
|---|---|---|
| H | 0 `KobBid` (Q); 1 P's A UTXO | 0 slot 0: 1000 A to Q (claimed); 1 **1.00 KAS to P2PK(M)** (unclaimed); 2 P's KAS change |

Merchant gain over unclaimed outputs = 1.00 KAS = `amount`. Accepted.

Aliasing attack X: P names M's own ask as a second leg and buys M's tokens with the bid's proceeds.

| | Inputs | Outputs |
|---|---|---|
| X | 0 `KobBid` (Q); 1 `KobAsk` (maker M); 2 P's A UTXO; 3 the ask's custody (1000 B, owner = ask id) | 0 slot 0: 1000 A to Q (claimed); 1 slot 1: **1.00 KAS to P2PK(M)**, the ask's payout to its maker (claimed); 2 1000 B to P; 3 P's KAS change |

Output 1 pays `payToScriptPublicKey` exactly `amount`. A verifier that applies the binding's rule
("exactly one output whose script equals `payToScriptPublicKey`, value exactly `amount`") to all
outputs accepts X, and the merchant releases its resource. But the 1.00 KAS is the price of M's own
1000 B, which left M's custody in the same transaction: M's balance sheet is unchanged, and P has
paid nothing beyond converting A into B at M's price. The output looks like a payment because the
ask's script forces the maker's payout to M's key, not because P paid M anything.

The exclusion rule of section 8.2 removes output 1 from the merchant-gain computation (it is the
positional slot of input 1). Unclaimed outputs are 2 and 3; none pays M; the gain is 0, not 1.00
KAS: rejected as `route_aliasing`. (The allowlist of section 8.4 also rejects output 2, since a
payer-owned output of B is not payer change of A. The exclusion rule does not depend on that: it is
the rule that stays correct if a deployment admits more payer outputs.)

Token variant. M wants token B and is the maker of a `KobBid` for B. P, who holds B, names M's bid as
the leg and sells B into it: the bid's positional slot delivers B to M as a KCC-20 UTXO owned by M,
borrow disabled, which can be made byte-identical to the merchant output (same amount, and a slot
value equal to `carrier`), while the bid's KAS released to P comes from M's own escrow. M has
exchanged escrowed KAS for B at its own bid price; P was not charged for the resource. Exclusion
of the slot leaves no unclaimed merchant output: rejected.

Consequences. A merchant who is also a maker gets a conservative outcome: a fill of its own order
never counts toward its price, and it must receive `amount` at a separate unclaimed output. This is
intended.

## 9. Price semantics

- **Named orders only.** The price guarantee is exactly what the named orders enforce (O1). The
  extension does not discover, rank or choose orders, and offers no price guarantee against the
  book at large.
- **The signed transaction fixes the payer's cost.** Every input and output is covered by the
  payer's SIGHASH_ALL, so what the payer spends (A units leaving its inputs, KAS carriers, fee) is
  determined exactly by the transaction it signs; if the orders change before broadcast, the
  transaction becomes invalid, it does not become more expensive.
- **The payer's maximum spend is a client-side check.** The requirement states what the merchant
  receives. A client MUST compute what the payer spends (Σ payer inputs minus Σ payer-controlled
  outputs, per asset, plus fee), MUST compare it against the user's or the agent's limit before
  signing, and MUST display it; it MUST NOT present `amount` as the cost. The server and the
  facilitator never see the payer's limit.
- **Spread.** As in section 6: the crossing spread goes to the transaction builder, the payer.

## 10. Conflict handling

Orders are shared UTXOs. A matcher or another taker may spend a named order before the payment is
accepted; then the payment cannot be accepted, and a payer input can likewise be spent elsewhere.

- The losing transaction is simply not accepted. Nothing of the merchant's is at risk.
- When verification or broadcast finds a named order (or payer input) missing from the UTXO set or
  conflicting, the facilitator returns the public reason `invalid_transaction_state` with local
  diagnostic `order_conflict`, `retryable = true`, and `extensions.kaspa.details.orders`: the array
  of order outpoints, in the binding's `{ "txid", "index" }` form, that are not spendable.
- **The payer re-quotes and re-signs.** The facilitator can never rebuild the transaction for the
  payer: SIGHASH_ALL covers every input and output, so substituting an order (an input) or
  redirecting an output invalidates the payer's signatures. The KOB negative tests demonstrate it
  (`crates/kob-tests/tests/kob_gap_tests.rs`, `gap_swap_resign_required`): SWR1, the facilitator
  swaps the named ask outpoint after signing, and the payer's token-leader signature no longer
  verifies; SWR2, the merchant B output is redirected after signing, and validation fails. No
  ANYONECANPAY or SINGLE hash type is allowed, precisely so that this holds.
- A re-signed payment is a new logical attempt. The binding's retry and signer policy applies: a
  corrective response is a new offer, not permission to sign again automatically, and a replacement
  requires explicit caller or wallet authorization (for example within limits the user pre-set:
  origin, recipient, maximum spend). The client MUST retain the failed attempt's input outpoints.
  If the replacement reuses at least one input of the failed attempt, the two attempts are mutually
  exclusive on chain and the payer cannot pay twice. If it does not, the client MUST first obtain
  the binding's permanent-absence proof (a trusted, confirmed spend of a persisted outpoint by a
  different transaction, for example the conflicting order spend, or its own revocation, section 13).
- A facilitator MUST NOT cache `order_conflict` (or any verification failure that consumed nothing)
  as the idempotent outcome of a `payment-identifier`. It SHOULD, after it definitively abandons a
  transaction, discard the bytes, never rebroadcast it, and keep its transaction id marked dead.

## 11. Completion

A routed payment is complete only when the **merchant output is in the accepted state** (in the
virtual UTXO set of the trusted node), optionally with the confirmation depth of the offer's
`extra.finality` (companion proposal, section 9.1: `confirmed` is accepted plus the operator's DAA
depth, reference 100). Mempool presence, a successful broadcast and a node's "already known" are not
completion. This matters more here than for a direct payment: a transaction in the mempool that
names contended orders can lose to a rival transaction, and only acceptance settles which one won. A
reorg after acceptance can replace the transaction by a rival fill of the same order; servers that
release irreversible goods against a routed payment SHOULD require `confirmed`.

## 12. Quote step (informative, not part of the wire)

Choosing the orders is the payer client's job, before it builds the transaction: it reads the book
of the pay asset and, for token B, of B, plans the legs that produce the merchant's `amount` at the
best price within the payer's limit, and builds and signs. Any KOB indexer's read API can supply the
orders (their outpoints, redeem scripts and states come from the placement records of their genesis
transactions). The server and the facilitator provide no quote endpoint and MUST NOT expose an
unauthenticated endpoint that reserves or awaits state. The facilitator's only involvement in this mode is
`/verify` and `/settle`. Any quoting or planning service is separate from this binding; the reference
quoter is `kob_executor::x402::quote` (best price first, plain orders only, skipping orders named in an
`order_conflict`). In intent mode (section 17) the facilitator chooses the orders itself, from its own
book, at execution time.

## 13. Carrier, minimum amounts, fees, expiry

### 13.1 Carrier and minimum amounts

- **KAS merchant.** The merchant output's value is `amount`. KIP-9 storage mass charges an output
  of value `v` up to `C / v` grams (`C = 10^12`) before the input-side credit. The node's relay floor
  does not price storage mass (13.2), but consensus bounds it per transaction by the block storage
  limit (500,000 grams), so an output below about 0.02 KAS cannot be paid at all, and block templates
  rank a transaction by its fee over its normalized mass including storage (a small output lowers
  its priority under contention). The binding defines no universal minimum and neither does this
  proposal; a server SHOULD set a minimum amount for routed entries as a labelled application policy,
  and SHOULD prefer `batch-settlement` for micro-prices.
- **Token merchant.** The merchant token output carries exactly `extra.token.carrier` sompi
  (companion proposal, section 3); the server chooses it (the reference minimum is 1 KAS).
- **Builder rules.** A builder MUST NOT create a payer KAS change output that pushes the transaction
  over the block storage limit and SHOULD NOT create one smaller than its minimum change (reference:
  0.01 KAS for a direct payment, 0.1 KAS for a routed one): it folds a smaller remainder into the fee
  if the fee bound allows, or selects another funding input.
  A builder SHOULD avoid a token change output whose carrier is below 1 KAS for the same reason.
  Payer change is the payer's cost; the verifier only requires the outputs to be payer-controlled.

### 13.2 Fee guidance

The fee floor is the relay floor of a rusty-kaspa v2.1.0 node, `100 sompi per gram × max(compute
mass, normalized transient mass)`, the normalized transient mass being `2 × size` with the mempool's
mass cofactors (`mining/src/mempool/check_transaction_standard.rs`). Storage mass is not part of it.
The reference builders pay exactly this floor by default and offer a storage-inclusive fee,
`100 × max(compute, 2 × size, storage)`, as an explicit priority option; a verifier MUST accept any
fee the node would relay (it must never be stricter than the node). Measured on the KOB test harness
(`gap_swap_and_pay`), the size of the transaction dominates because every token input carries its
whole program in its signature script:

| Shape | Legs | Size | Floor fee |
|---|---|---|---|
| SW1 | 1 bid, merchant KAS | about 4.8 kB | about 0.0096 KAS |
| SW3 | 1 bid, 1 ask, merchant B | about 9.7 kB | about 0.0193 KAS |
| SW4 | 2 bids, 3 asks, merchant B (largest under the default 3/3 token program) | about 19.6 kB | about 0.0393 KAS |

The payer pays the fee. A verifier bounds the fee (reference default 0.5 KAS) and rejects a fee below
the recomputed floor; a facilitator MAY require a higher rate.

### 13.3 TTL and payer-side revoke

`authorization.expiresAt` follows the binding's expiry rule (strictly after now, at most now +
`maxTimeoutSeconds`). Orders move quickly, so a server SHOULD set `maxTimeoutSeconds` to 60 or
less, and a client SHOULD sign and submit immediately. Expiry limits when a facilitator will accept
the payment; it does not expire the transaction, which stays valid while its inputs are unspent
(companion proposal, section 9.4). The payer's **revoke** is a self-spend: spend one payer input in
another transaction. Because payer inputs are P2PK-owned or borrow-disabled token UTXOs, only the
payer can do it. A client SHOULD revoke a disclosed routed payment that expired without acceptance
evidence; the revocation also supplies the permanent-absence proof of section 10.

## 14. Replay and idempotency

As in the binding and the companion proposal: the `payment-identifier` extension is required and
bound to the request fingerprint and the merchant-gain profile; a transaction id is consumable at
most once per trust domain; the replay ledger consumes every outpoint the transaction spends,
including every named order outpoint, before broadcast, so two payments cannot spend one payer UTXO
or one order for two requests. The commitment digest includes `route.payAsset`, so an authorization
for one pay asset cannot be reused for another. Ambiguous outcomes stay consumed.

## 15. Interaction with the exact profiles

| Topic | Rule for routed entries |
|---|---|
| `standard-native` (binding) | The binding's canonical transaction (version 0, P2PK-only inputs, no covenants) does not apply to a routed entry; section 7 replaces it. The merchant-gain rule is the binding's, over unclaimed outputs only. |
| `kcc20` (companion) | The merchant token output, `tokenScriptPublicKey`, carrier, borrow-disabled rule and custody class are as in the companion proposal; the payer's inputs are tokens of A instead of B. Custody class in `extra.token` describes B, what the merchant receives. |
| `additive` | Not combinable. |
| Authorization | Payload commitment with `route` set. The binding's signed authorization is not used for routed entries. |
| `accepted` equality | `payload.accepted` MUST equal the offer (canonical JSON), including `route`. |
| Direct payments | A direct entry and a routed entry for the same price are separate `accepts` elements; a client picks the one it can satisfy. |

## 16. Security considerations

- **Aliasing.** Section 8.6. The rule is: never count an output the order covenants force. Any
  implementation that reuses the binding's merchant-output rule on all outputs of a transaction with
  order inputs is vulnerable.
- **The facilitator cannot steal or redirect.** It holds no keys, and each payer signature covers
  every input and output. It can refuse, delay, or drop a payment. It can alter fields outside the
  signature hash (signature scripts, compute budgets, committed storage mass); the verifier's
  recomputation and engine validation cover those, and the facilitator broadcasts exactly the
  verified bytes.
- **Front-running.** A matcher that sees a pending routed transaction can fill the named orders
  first. The payer loses nothing (the payment simply fails) and the merchant loses nothing. The
  cost is retries. A third party cannot change the payer's outputs or capture value from it.
- **Contended orders as a resource attack.** A payer can name orders that will certainly conflict to
  waste verification work. Rate-limit per source and per merchant before chain lookups, bound the
  number of inputs, outputs and signature-script bytes before running the engine (reference: 32
  inputs, 16 outputs, 32 KiB per signature script, 256 KiB artifact), and never let unauthenticated
  requests create ledger state (the one exception, the invoice payment endpoint of section 18.3, creates it only
  for a payment that verifies completely against a registered, unexpired, unpaid invoice).
- **Lookalike tokens.** Every token of the transaction is pinned by covenant id, template hash and
  extension commitment (companion proposal, section 13.1). `payAssets` pins A; B is pinned by
  `extra.token`; order custody tokens must be allowlisted tokens.
- **Custody.** The merchant's exposure is the class of what it receives; A's class does not affect
  the merchant. A server MAY accept issuer-controlled pay assets for that reason. An issuer of A
  can still freeze the payer's UTXOs before acceptance; the payment then fails. Classes are those
  of the companion proposal, section 4 (a property of the pinned program: the reference KCC-20 and
  KRON are `unconditional`, KaspaCom's template, with mint, public mint and burn, is
  `issuer-controlled`). The class of a token the merchant receives (`extra.token.custody`) is always
  stated in the offer and gated by the operator; an operator that accepts an issuer-controlled pay
  asset says so explicitly, and the pay asset's class is not part of the payment.
- **Soft expiry of orders.** An order past its soft expiry can still be filled at its own limit
  until it is refunded; the covenant guarantees the limit, so a late fill is not a loss.
- **Privacy.** The named orders and their makers are public on chain.
- **Wallet limits.** As in the companion proposal (section 13.6, informative): few wallets sign
  covenant inputs; the KOB client builds the transaction itself and asks the wallet only for
  Schnorr signatures over the payer's inputs.

## 17. Intent mode (`kob-intent-v1`)

This section proposes a second mode of the same extension. In the mode of sections 3 to 16 the payer
signs a transaction over named orders, so a conflicting fill makes the payer quote and sign again
(section 10). In intent mode the payer signs once, a transaction that locks its funds in an **intent**:
a covenant UTXO whose terms bind the merchant's output and the payer's worst case and which any party may
spend in a transaction that satisfies those terms, without the payer. The facilitator verifies the
intent against the offer, broadcasts it and then **executes it itself** against the book of the moment,
acting as the keeper; when an order it chose is taken first it chooses again. The payer can always
cancel an intent that was not executed.

The mode needs an intent program with the properties below. KOB provides one, its router
(`contracts/argent/kob_router.ag`, informative, section 17.3); another order book would bring its own.

### 17.1 Requirements

An intent offer is an exact entry of the merchant-gain profile (as in section 3) whose `extra.route` is:

```json
"route": {
  "binding": "kob-intent-v1",
  "critical": true,
  "router": "<id of the intent program artifact the facilitator executes, 64 hex>",
  "payAssets": [ { "asset": "<covenant id>", "templateHash": "<64 hex>", "extensionCommitment": "<64 hex>" } ]
}
```

- `binding` MUST equal `kob-intent-v1` and `critical` MUST be `true` (section 3 applies unchanged: a
  client that does not implement the mode skips the entry).
- `router` names the intent program the facilitator executes (KOB: the id of the router's Argent
  artifact, `ROUTER_ARTIFACT_ID` in `crates/kob-protocol/src/router.rs`, also advertised in `/supported`).
  A payer MUST NOT create an intent for a program
  other than one it recognizes; a facilitator MUST reject an offer naming a program it does not execute.
- `payAssets` is as in section 3, plus `{ "asset": "KAS" }` when the merchant receives a token. Every token
  of the offer (the pay assets and the merchant token) MUST be one the intent program can trade.
- The merchant-gain fields (`asset`, `amount`, `payTo`, `extra.token`) are as in section 3. `payTo` MUST
  be a Schnorr P2PK address: the intent pays its key.
- `maxTimeoutSeconds` bounds how long the facilitator executes the intent (section 17.6). Because an
  intent survives conflicts, a server MAY choose it longer than for the payer-signed mode (KOB examples
  use 300 to 600 seconds).

### 17.2 The intent program (abstract)

An intent program is a covenant whose UTXO the payer funds once and which has two kinds of spend:

- **(I1) Execution.** Anyone may spend it in a transaction that pays the merchant exactly what the terms
  say, at an output position the program pins, and that keeps the payer within its worst case (the KAS it
  may pay, or the token units it may sell). The program reads the orders it is filled against by their
  pinned templates and checks the token flows; the orders validate themselves as in section 4.
- **(I2) Cancel.** The payer's key may spend it at any time (SIGHASH_ALL) and take everything back.
- **(I4) Expiry.** The program carries a **deadline**; from it on anyone may spend the intent in a
  transaction that returns everything to the payer (less a bounded fee), proven by a lock time. The
  expiry, not the clock, is what ends an intent nobody executed (section 17.7).
- **(I3) Positional anti-aliasing.** The outputs the program counts as the merchant's and the payer's are
  outputs no other input of the transaction can claim (KOB: the intent is the last input `j` and owns
  outputs `j` and `j + 1`, see `docs/argent.md`, "Positional anti-aliasing rule"). This is the intent
  counterpart of section 8.2: a merchant who is also a maker can never be "paid" by its own order.

Because the program checks the payment itself, a facilitator that builds the execution cannot redirect it,
and the execution needs no payer signature. What an order releases beyond what the payer authorized (a
crossing spread, a tip, the unused KAS of a token intent) belongs to whoever executes.

### 17.3 KOB instantiation (informative)

The KOB router has one actor (one template) per fill shape, so the payer chooses the shape when it creates
the intent:

| Pay asset | Merchant receives | Intent | Terms (the actor's state) |
|---|---|---|---|
| KAS | token B (`kcc20`) | `KasToToken_<shape>` (1 to 3 `KobAsk`s of B) | payer, merchant, B, exact `amount`, `max_pay` (sompi at the asks' quotes: `ceil(take × price / scale)` per ask), `max_extra` (carriers and fee), `deadline` |
| token A | KAS (`standard-native`) | `TokenToKas_<shape>` (1 to 3 `KobBid`s of A) | payer, merchant, A, `merchant_kas` (at least), `max_sell` (base units of A), `deadline` |
| token A | token B (`kcc20`) | `TokenSwap_<shape>` (1 or 2 bids of A and asks of B) | payer, merchant, A, B, `max_sell_a`, exact `amount_b`, `deadline` |
| KRON token A | KAS / token B | `TokenToKasKron_<shape>` / `TokenSwapKron_<shape>` (`KobBidKron`s of A) | as `TokenToKas` / `TokenSwap` |

`deadline` (unix ms) is the payer's `authorization.expiresAt`: the payload carries no separate field,
the verifier takes it from the authorization (section 17.5).

A shape says how many orders of each leg are filled and whether the last one rests or ends
(`KasToToken_buy`: one ask that continues; `TokenToKas_sell2_out`: two bids that both end; and so on).
A one-order resting shape (`KasToToken_buy`, `TokenToKas_sell`, `TokenSwap_swap`) is the one most likely
to find another order after a conflict, and is the KOB SDK default. A token intent locks the payer's
token A in a separate token UTXO owned by the intent's covenant id; the intent's own KAS is what the keeper
may spend on fillers and the fee. The router links the orders by closed ICC and reads the tokens under the
**program handles the intent's state names** (open ICC): every token is named by covenant id and program,
so the terms the verifier recomputes include the program of every token (the one the offer's `templateHash`
and the allowlist give). An intent trades any KCC-20 program of the 112-byte state (the 3/3 reference
program, the 8/8 program KOB issues, ...) and the KRON programs; a program's own slot limits bound the
shapes it runs (the 3/3 program has no three-bid sell). A KRON token is a pay asset only (`TokenToKasKron_*`,
`TokenSwapKron_*`, sold into `KobBidKron`s): its lock is a KRON UTXO of `id_type` 2, its deliveries and the
payer's change are `id_type` 3 (address presence), and because a KRON output holds at least one unit the lock
exceeds `maxSell` (`lockAmount` > `maxSell`); the creation spends the payer's key-held KRON tokens next to a
P2PK input of their owner. KaspaCom's program is paid through the payer-signed mode while it is pending review.

### 17.4 Payload

The payload is the `exact-transaction` payload of section 5 with:

- `transaction`: the signed **creation** transaction (safe-json, as in the companion proposal). It is not
  broadcast by the payer (section 17.6).
- `paymentOutputIndex`: the index of the intent output in the creation.
- `route`:

```json
"route": {
  "binding": "kob-intent-v1",
  "payAsset": "<covenant id, or KAS>",
  "orders": [],
  "intent": {
    "actor": "TokenToKas_sell",
    "payer": "<x-only key of the payer, 64 hex>",
    "maxSell": "3000",
    "lockOutputIndex": 1
  }
}
```

  `intent` carries what the verifier needs besides the offer to recompute the intent's script: the actor,
  the payer's key and its worst case (`maxPay` and `maxExtra` for a KAS pay asset; `maxSell`, the index of
  the locked token output and, when it differs from `maxSell`, `lockAmount` for a token pay asset). `orders`
  is empty: the keeper chooses the orders.
- `authorization`: the payload commitment of the companion proposal with
  `route = { "binding": "kob-intent-v1", "payAsset": "<...>", "actor": "<actor>" }` inside the digest object,
  embedded in the **creation's** transaction payload as its single `X402` record. Every payer signature of
  the creation covers it (companion proposal, section 6.4).

### 17.5 Verification

The verifier recomputes everything from the offer and the trusted chain view:

1. The envelope as in the binding (accepted equals the offer, network, amount, `payTo`, request hash),
   the intent offer (binding, critical flag, the program it executes, pay assets) and the payload's route
   (`kob-intent-v1`, a listed pay asset, no orders, terms present).
2. The pay asset and the intent kind it implies: `KAS` only when the merchant receives a token; a token
   pay asset allowlisted, on a program the intent program trades, issuer-controlled only when the operator
   enables it (the class of what the merchant receives is stated in `extra.token` as in section 3).
3. The intent's terms recomputed from the **offer** (merchant key from `payTo`, merchant asset, exact
   amount), the payload's `intent` (payer key, worst case) and the authorization (the deadline is
   `expiresAt` in unix ms), and from them the intent's script. The
   output at `paymentOutputIndex` MUST carry exactly that script, and be a single-output covenant genesis
   authorized by a payer P2PK input. No other output may carry the intent's covenant id. For a token
   intent the output at `lockOutputIndex` MUST be the pay token owned by the intent's covenant id, holding
   `lockAmount` (at least `maxSell`) units, borrow disabled, the pinned extension.
4. The creation: bounded parse, recomputed id, trusted UTXOs of every input (all unspent), every input a
   payer P2PK input or a payer key-owned token of the pay asset (borrow disabled, pinned program and
   extension), every payer signature SIGHASH_ALL (section 8.1a; `token_owner_scheme`), engine
   validation, fee and mass as in the binding.
5. The commitment: the recomputed digest equals `authorization.digest` and the creation's single `X402`
   record; expiry as in the binding; the `payment-identifier` as in section 14.

An intent for another merchant, another amount or another asset therefore fails at step 3; an intent
presented for another offer or request fails at step 5 (its commitment names the original ones). A
verifier SHOULD also build an execution against the current book before it reports the payment valid for
settlement (section 17.6): an intent no order can fill is not worth broadcasting.

### 17.6 Settlement

1. The facilitator verifies (section 17.5), then **dry-runs** an execution against its book: if none
   builds, it answers `intent_not_executable` (retryable) and broadcasts nothing; nothing is consumed.
2. It consumes the creation's inputs and the intent outpoint in its replay ledger (section 14), then
   broadcasts the creation.
3. Once the intent UTXO is accepted it plans an execution against the book of the moment, records it
   durably, and submits it. When an order it named was filled by someone else (the submission conflicts,
   or the order is spent while the execution is not accepted) the execution is dead: the facilitator
   remembers the order as lost and plans again, without the payer. It does not execute after the
   **deadline**: `authorization.expiresAt`, capped by an invoice's `expiresAt` (section 18); it then expires the intent (section 17.7).
4. Completion is the execution's merchant output at the required finality, exactly as in section 11. The
   settlement response names the **execution** in `transaction`, its merchant output in
   `extensions.kaspa.paymentOutputIndex`, and the creation in `extensions.kob.intent`
   (`{ "creation", "outpoint", "executions" }`).
5. A `/settle` that ends before completion answers `settlement_pending` (retryable); the facilitator keeps
   executing in the background until completion or the deadline, and an identical retry resumes.
6. When the deadline passes with nothing in flight the payment ends `intent_expired`; when the intent was
   spent outside the facilitator (the payer's cancel, or another party's execution, which pays the merchant
   too) it ends `intent_spent`.

**The payer MUST NOT broadcast the creation itself.** Like a payer-signed transaction, a creation is a
bearer instrument until a facilitator has consumed it: anyone who sees it on chain and can obtain the
offer could present it for the same resource first. A facilitator therefore requires the creation's
inputs unspent (a creation already on chain is refused like any spent input), and the payer hands the
unbroadcast creation to the facilitator only.

### 17.7 Cancel, deadline and expiry

The intent's deadline is the authorization's expiry (`expiresAt`, unix ms); a payer paying an invoice
chooses its authorization's expiry no later than the invoice's (KOB payer SDK: `IntentOptions::expires_in_ms`).

- **Cancel.** The payer can cancel at any time (I2). Before the deadline a cancel races the
  facilitator's execution and either may win, never both. The KOB router requires a token intent's
  cancel to spend the locked tokens at input `j + 1`, as the one token input of that token, owned by the
  intent (it reads them under the intent's program handle): tokens owned by the intent's id could never
  move after the cancel.
- **Expiry.** From the deadline on, anyone may expire the intent (I4). The KOB router's `expire`
  entry needs no signature: it requires the transaction's lock time to be at least the deadline
  (CLTV, a time lock; a node accepts it once its past median time reached the deadline), returns the
  intent's KAS to the payer's key less at most `EXPIRE_MAX_FEE` (0.1 KAS) at the intent's output
  position `j`, and, for a token intent, the locked tokens, whole and borrow disabled, to the payer's
  key at `j + 1` (the input at `j + 1` must be those tokens).
- **The facilitator** stops executing at the deadline (section 17.6), reports `intent_expired`, and
  then expires the intent on chain itself, retrying until the expiry is accepted (it is a non-final
  transaction until the node's past median time reaches the deadline) or the intent is spent
  otherwise. Once the expiry is accepted nobody can execute the intent; if another transaction spent
  it, the facilitator tells the payer's cancel from a late execution by the spending transaction. KOB's
  facilitator pays the expiry at its normal fee rate and, when it still waits in the mempool a minute
  later, replaces it (replace-by-fee) at its high rate, within `EXPIRE_MAX_FEE`.
- **Without the facilitator.** The expiry needs no signature, so the payer (or any keeper) can build and
  submit it as well (KOB payer SDK: `expireIntent`), e.g. when the facilitator that took the payment is
  gone; the payer's cancel remains the other way out.

What a deadline cannot do is refuse an execution by the clock: a covenant can prove that a
transaction is not earlier than a time (a lock time), never that it is not later. Between the
deadline and the expiry's acceptance a third party can still execute the intent; the merchant is then
paid for a request the facilitator reported as `intent_expired` (a late payment, as in section 18 for
invoices). The window is the time the facilitator needs to get its expiry accepted after the deadline.

### 17.8 Price semantics

The payer's worst case is in the intent's terms and is enforced by the intent program: at most `max_pay +
max_extra` sompi (KAS pay asset; the rest of the intent's KAS returns to the payer at execution), or at
most `max_sell` units of A plus the intent's own KAS (token pay asset). A client MUST show this worst case,
not `amount`, as the cost. Within it, the executor keeps what the orders release beyond the merchant's
amount; a payer that wants a tighter price sets tighter terms. As in section 9, the extension discovers
nothing: the price guarantee is the terms. The router counts what a `KasToToken` execution may take per ask
at the ask's quote, `ceil(take × price / scale)` sompi for `take` base units (the covenants' maker-favour
rounding, exact split multiplication), so a decaying ask, a tip or a crossing spread only lowers what the
ask really demands (`ceil(take × (price(t) − tip) / scale)`); fills are any base-unit amounts that respect
each order's minimum fill.

### 17.9 Security considerations

- **No redirect.** The intent program checks the merchant's output and the payer's change; the facilitator
  can refuse, delay or execute, not redirect (I1, I3).
- **Binding.** The offer fixes the terms the verifier recomputes; the commitment, covered by every payer
  signature of the creation, fixes the offer, the request and the expiry. The intent outpoint is consumed
  once.
- **Third-party keepers.** An intent is a public covenant: any party may execute it, and the merchant is
  paid either way. A facilitator that finds its intent spent by a transaction it did not submit reports
  `intent_spent`; it cannot tell a third-party execution from the payer's cancel without the spending
  transaction. After the deadline the facilitator's own expiry ends the intent (section 17.7).
- **Front-running a creation.** Section 17.6.
- **Resource cost.** The dry run builds and validates up to a bounded number of candidate executions per
  request; rate-limit as in section 16.

### 17.10 Diagnostics

| Diagnostic | Public reason | Retryable | Raised when |
|---|---|---|---|
| `intent_not_executable` | `invalid_transaction_state` | yes (dry run), no (attempts exhausted) | The book cannot execute the intent now (dry run: nothing was broadcast), or every execution attempt failed. |
| `intent_expired` | `invalid_transaction_state` | no | The deadline passed without an accepted execution; the facilitator expires the intent (the payer can also cancel it). |
| `intent_spent` | `invalid_transaction_state` | no | The intent was spent outside the facilitator. |

`route_unsupported` (section 20) also covers an offer whose `router` the facilitator does not execute, a
token off the intent program's token program, and an actor of another intent kind.

### 17.11 Conformance cases

Engine-validated cases: `crates/kob-protocol/tests/intent_builders.rs` (every router shape created,
executed and validated on every program pair of its family: 8/8, 3/3, 3/3 to 8/8, both KRON programs;
redirected outputs rejected by the router; the payer's cancel; the cancel budget; every shape expired at
its deadline, rejected 1 ms before it and with its KAS or tokens redirected),
`crates/kob-tests/tests/c4_security_tests.rs` (the cancel's lock owner check on KCC-20 and KRON, with its ablation),
`crates/kob-x402/tests/intent.rs` (payer SDK, verifier and keeper over the in-memory chain; `intent-neg-hashtype`
in `intent_creation_payer_signatures_must_use_sighash_all`) and
`crates/kob-executor/tests/x402_intents.rs` (the facilitator end to end). The router's own covenant rules
are covered by `crates/kob-tests/tests/argent_router_tests.rs` (positive, negative and ablation cases).

| Id | Case | Expected |
|---|---|---|
| `intent-pos-t2k` | token A locked in a `TokenToKas` intent, executed into a bid | valid; merchant gets `amount` KAS |
| `intent-pos-k2t` | KAS locked in a `KasToToken` intent, executed from an ask | valid; merchant gets `amount` of B |
| `intent-pos-swap` | token A locked in a `TokenSwap` intent | valid |
| `intent-pos-conflict` | the first execution loses its order to another fill; the facilitator re-plans | settled, one payer signature, `executions` = 2 |
| `intent-neg-merchant` | the creation's intent pays another key than `payTo` | `invalid_kaspa_exact_payment_output` |
| `intent-neg-offer` | the payload presented for another offer or request | `invalid_kaspa_x402_accepted` / `invalid_authorization` / `invalid_kaspa_x402_payload` |
| `intent-neg-terms` | `maxSell` changed after signing | `invalid_kaspa_exact_payment_output` |
| `intent-neg-actor` | the actor changed after signing; an actor of another kind | `invalid_authorization`; `route_unsupported` |
| `intent-neg-hashtype` | a correctly signed creation whose P2PK input or token owner witness uses SIGHASH_NONE, SIGHASH_SINGLE or an ANYONECANPAY form, or a trailing byte that is no hash type | `token_owner_scheme` |
| `intent-neg-broadcast` | the payer broadcast the creation before submitting it | `invalid_kaspa_exact_utxo` |
| `intent-neg-program` | a pay token off the intent program's token program | `route_unsupported` |
| `intent-neg-book` | no order fits the intent's shape and limits | `intent_not_executable`, nothing broadcast |
| `intent-end-deadline` | nothing executable until the deadline; the payer cancels | `intent_expired`; the cancel returns everything |
| `intent-end-expiry` | nothing executable until the deadline; the payer does nothing | `intent_expired`; the facilitator's expiry returns the KAS (less at most `EXPIRE_MAX_FEE`) and the tokens to the payer |
| `intent-neg-expiry-early` | an expiry with a lock time before the deadline | rejected by the router (lock time) |
| `intent-pos-programs` | every intent on the 3/3 reference program and across programs (a 3/3 token A sold for an 8/8 token B) | valid; the 3/3 program refuses a three-bid sell at creation |
| `intent-pos-kron` | a KRON token A (2,433 B and 2,732 B programs) locked in a `TokenToKasKron` / `TokenSwapKron` intent, sold into `KobBidKron`s | valid; the payer's change and the bid deliveries are `id_type` 3 |
| `intent-neg-kron-lock` | a KRON intent whose `lockAmount` is not above `maxSell` | `invalid_kaspa_x402_payload` |
| `intent-neg-program` (KRON) | a KRON token as the merchant asset; KaspaCom's program as a pay asset | `route_unsupported` |
| `intent-neg-cancel-lock` | a payer-signed cancel that spends other tokens of the locked token at `j + 1` instead of the lock | rejected by the router (the observed lock is not owned by the intent) |

## 18. Invoices (`kob-invoice-v1`)

An invoice is what a merchant hands a payer who is not the HTTP client of a resource: a point-of-sale
screen, a QR code, a payment link. It is the x402 requirements of one sale plus the merchant's reference
and an expiry. This proposal keeps it to what a facilitator needs to settle it once and report it; how a
merchant displays it is outside the proposal.

### 18.1 Object and id

```json
{
  "x402Version": 2,
  "invoiceVersion": "kob-invoice-v1",
  "network": "kaspa:testnet-10",
  "reference": "order-1234",
  "expiresAt": "2026-10-01T12:00:00.000Z",
  "memo": "2 coffees",
  "accepts": [ { "scheme": "exact", "...": "exact requirements of any profile, routes included" } ]
}
```

- `reference` is the merchant's own identifier (1 to 128 printable ASCII characters); `memo` is optional
  display text (at most 280 characters).
- `expiresAt` follows the binding's timestamp form. No payment is settled at or after it.
- `accepts` lists 1 to 16 distinct exact requirements on `network`; any one of them pays the invoice. A
  merchant can accept KAS directly, a token, payer-signed routes and intent routes in one invoice.
- Unknown members are refused.
- The **id** is the SHA-256 of the invoice's canonical JSON (the binding's canonical JSON, as for the
  requirements hash), 64 lowercase hex.

### 18.2 URL and registration

A facilitator that serves invoices serves a registered invoice at `GET <base>/invoices/<id>`; **that URL is
what a QR code or a link carries**. Because the id is the content hash, the URL is self-verifying: a payer
MUST recompute the id of what it fetched and refuse a mismatch, so the host cannot alter the terms.

Registration is `POST /invoices` with the merchant's API key (the same authentication as `/settle`). The
facilitator validates the invoice, checks that it can settle every entry (profile, tokens, routes) and that
every entry's `payTo` and asset are the merchant's, and stores it; registering the same content again is
idempotent. Registration is at the facilitator rather than merchant-hosted because the facilitator needs
state anyway (one payment per invoice, the status) and already authenticates merchants; a merchant MAY
still host the same JSON elsewhere, and a payer verifies it the same way (the id is the content hash).

### 18.3 Paying

The payer selects one entry of `accepts`, builds a payment for it exactly as for that entry alone, with
**the invoice id as the request hash** (so the authorization commits to the invoice, its reference and its
expiry), and submits the `PaymentPayload` to `POST <base>/invoices/<id>/pay`, without an API key. The
facilitator settles it for the invoice's merchant and answers a `SettlementResponse` (an intent entry may
answer `settlement_pending` while it executes; the status then follows it). This endpoint is the one
unauthenticated path to the facilitator's ledger: it is rate-limited per source like `/verify`, and it
creates ledger state only for a payment that verifies completely against a registered, unexpired, unpaid
invoice.

### 18.4 Once only; duplicates and late payments

Under a per-invoice lock: a payment whose transaction is already an attempt of the invoice resumes (an
identical retry); otherwise a payment while an attempt is in flight is refused as `invoice_pending`
(retryable), one after an accepted attempt is a **duplicate** (`invoice_paid`), and one at or after
`expiresAt` is **late** (`invoice_expired`). A refused payment is never broadcast by the facilitator. It is
verified first (a forged one is just rejected) and kept as evidence with the output it would pay the
merchant (or its intent outpoint), and the facilitator keeps watching that output: when a refused payment
reaches the chain anyway (its payer broadcast it), the invoice status reports it as `accepted`, so the
merchant can refund it (an intent of a refused payment that was spent outside the facilitator is
reported as `spent`). Refunds themselves are outside this proposal.

### 18.5 Status

`GET <base>/invoices/<id>/status` (public; a read, never an await):

```json
{
  "id": "<id>",
  "reference": "order-1234",
  "status": "paid",
  "expiresAt": "2026-10-01T12:00:00.000Z",
  "payment": { "transaction": "<the transaction that paid the merchant>", "acceptedDaaScore": "123456",
               "payer": "kaspatest:...", "acceptedIndex": 1, "response": { "...": "the settlement response" } },
  "attempts": [ { "transaction": "<txid>", "state": "accepted" } ],
  "extraPayments": [ { "kind": "duplicate", "transaction": "<txid>", "observed": "refused", "at": "..." } ]
}
```

`status` is `unpaid` (no attempt yet), `pending` (an attempt in flight), `paid` (an attempt reached the
required finality), `expired` (expired without a payment) or `failed` (the last attempt failed
definitively; the invoice can still be paid until it expires). For an intent, `payment.transaction` is the
execution and `attempts[].transaction` the creation.

### 18.6 KAS URI fallback

An invoice with a plain `standard-native` entry can also be paid by any Kaspa wallet through the URI
`<payTo>?amount=<KAS>` (the `kaspa:` address of the entry, the amount in KAS with up to 8 decimals). Such a
payment never reaches the facilitator: it has no commitment to the invoice, no once-only guarantee and no
invoice status, and the merchant matches it by watching its address. It needs no executor support.

### 18.7 Diagnostics

| Diagnostic | Public reason | Retryable | Raised when |
|---|---|---|---|
| `invoice_unknown` | `invalid_transaction_state` | no | No invoice has this id (HTTP 404). |
| `invalid_invoice` | `invalid_payment_requirements` | no | The invoice is malformed, too long-lived, or an entry is not settleable here. |
| `invoice_expired` | `invalid_transaction_state` | no | A payment at or after `expiresAt` (late; kept as evidence). |
| `invoice_paid` | `invalid_transaction_state` | no | The invoice is paid (duplicate; kept as evidence). |
| `invoice_pending` | `invalid_transaction_state` | yes | Another payment of the invoice is being settled. |

### 18.8 Conformance cases

| Id | Case | Expected |
|---|---|---|
| `invoice-id` | canonical id, key order irrelevant, the reference inside it | equal ids across implementations |
| `invoice-fetch` | content altered under the same id | refused by the payer |
| `invoice-pay` | one entry paid with the invoice id as request hash | `paid`, the payment's transaction |
| `invoice-pay-intent` | an intent entry paid; its first execution conflicts | `paid` with the execution |
| `invoice-dup` | a second payment after `paid` | `invoice_paid`, not broadcast, `extraPayments[duplicate]`; `accepted` if its payer broadcasts it |
| `invoice-late` | a payment after `expiresAt` | `invoice_expired`, not broadcast, `extraPayments[late]` |
| `invoice-wrong` | an entry not in `accepts`; a payment for another merchant; another request hash | `invalid_kaspa_x402_accepted`; `invalid_authorization`; `invalid_kaspa_x402_payload` |
| `invoice-deadline` | an intent entry not executable before `expiresAt` | `expired`; the intent ends `intent_expired`; the payer cancels |

## 19. Schema amendments

`schemas/kaspa-requirements-extra.schema.json`: add an optional `route` property to the exact
object:

```text
route: {
  binding:  const "kob-swap-v1",
  critical: const true,
  payAssets: array, minItems 1, unique by asset,
             items { asset: covenantId, templateHash: hash32, extensionCommitment: hash32 }
             (all three required, additionalProperties false), or { asset: const "KAS" }
             (only when the merchant receives a token)
}   additionalProperties false
```

Conditional: `route` is forbidden when `profile` is `additive`.

`schemas/kaspa-payment-payload.schema.json`: add an optional `route` property:

```text
route: {
  binding: const "kob-swap-v1",
  payAsset: covenantId or const "KAS",
  orders: array of outpoint (txid hash32, index uint32), maxItems bounded by the verifier limits
}   additionalProperties false
```

Conditional: when `route` is present, `profile` is `standard-native` or `kcc20`, and
`authorization.version` is `kob-x402-payload-commitment-v1` (companion proposal, section 12).
Transaction projection: output `covenant` objects and input `utxo.covenantId` as in the companion
proposal, section 7.1.

Intent mode (section 17): `route.binding` may also be `"kob-intent-v1"`, with a required `router` (hash32) in the
requirement and an `intent` object in the payload (`actor` string, `payer` hash32, optional `maxPay`, `maxExtra`,
`maxSell`, `lockAmount` uint64 strings, `lockOutputIndex` uint32; additionalProperties false); `orders` is then empty.
An invoice (section 18) is a new top-level schema: `x402Version` 2, `invoiceVersion` const `kob-invoice-v1`, `network`,
`reference` (1..128 printable ASCII), `expiresAt`, optional `memo` (at most 280), `accepts` (1..16 requirements);
additionalProperties false.

`GET /supported`: a facilitator that serves the extension adds `"kob-swap-v1"` to
`extra.routeBindings` of its `exact` kind and the tokens it accepts to `extra.tokens`; it MUST omit it
unless its order-program pins, token allowlist and engine validation are configured and healthy. A facilitator that
executes intents also lists `"kob-intent-v1"` and the `extra.router` it executes; one that serves invoices adds
`"invoices": "kob-invoice-v1"`.

## 20. Diagnostics added

Public reasons are the binding's closed set; the diagnostic is carried in
`extensions.kaspa.diagnostic` with `retryable`, `message` and optional `details`. Diagnostics
inherited from the companion proposal (`token_not_allowlisted`, `token_template_mismatch`,
`token_borrow_enabled`, `token_owner_scheme`, `underpayment`, `overpayment`, `carrier_mismatch`,
`invalid_authorization`, `expired_authorization`, `authorization_exceeds_max_timeout`) apply
unchanged.

| Diagnostic | Public reason | Retryable | Raised when |
|---|---|---|---|
| `route_unsupported` | `unsupported_scheme` (swap-and-pay not served); otherwise the reason of the failing part (`invalid_payload`, `invalid_payment_requirements`) | no | The server or facilitator does not serve swap-and-pay, or the route binding or shape is unknown (including a pinned order template of an unsupported kind). |
| `route_aliasing` | `invalid_payload` | no | The merchant's terms are met only by an order-claimed output (section 8.3), or an order input is not among inputs `0..k-1`. |
| `unknown_order_template` | `invalid_payload` | no | An input is a covenant that is not a pinned, supported order template. |
| `order_conflict` | `invalid_transaction_state` | yes | A named order (or payer input) is spent or missing; `details.orders` lists the order outpoints. The payer re-quotes and re-signs. |
| `order_not_spendable` | `invalid_transaction_state` | yes | An order input is unspent but its covenant rejects this transaction. Defined; not raised by the reference verifier (section 8.5). |
| `pay_asset_not_accepted` | `invalid_payload` (payload names an asset outside `payAssets` or the allowlist); `invalid_payment_requirements` (an offered pay asset is not allowlisted) | no | `route.payAsset` or a payer token input is not an accepted pay asset. |

Intent mode and invoices add the diagnostics of sections 17.10 and 18.7.

## 21. Test vectors

Not implemented: published vector files under `crates/kob-x402/vectors/swap/` (the directory does
not exist), in the format of the companion proposal (requirements, both canonical JSON preimages
and hashes, payload bytes, safe-json transaction, trusted UTXOs, recomputed id, expected diagnostic
and public reason for negatives). The cases below are engine-validated scenarios in
`crates/kob-tests/tests/kob_gap_tests.rs` (SW1 to SW4 and the re-sign negatives SWR1 and SWR2), in
`crates/kob-x402/tests/swap.rs` and `swap_families.rs` (verifier and payer SDK against an
engine-validated chain; the KRON and KaspaCom-template cases; `swap-neg-hashtype` in
`swap_payer_token_witness_must_use_sighash_all`, `swap_payer_token_delegator_witness_must_use_sighash_all`,
`swap_funding_input_signature_must_use_sighash_all` and `every_family_signs_with_sighash_all_only`), and through the facilitator in
`crates/kob-executor/tests/x402_swap_families.rs`.

| Id | Case | Expected |
|---|---|---|
| `swap-pos-sw1` | A to KAS through one bid | valid |
| `swap-pos-sw2` | KAS (`payAsset` `KAS`) to B through one or more asks | valid |
| `swap-pos-sw3` | A to B through one bid and one ask | valid |
| `swap-pos-sw4` | two bids and three asks | valid |
| `swap-pos-kron-sw1` | a KRON token to KAS through one `KobBidKron` | valid |
| `swap-pos-kron-sw3` | a KRON token to a KCC-20 token B through a `KobBidKron` and a `KobAsk` (one cross-family transaction) | valid |
| `swap-pos-kaspacom` | A (reference KCC-20) to a KaspaCom-template token B; a KaspaCom-template token A to KAS | valid |
| `swap-neg-kron-allowlist` | a KRON order or token that is not allowlisted; a wrong template hash or family for the pinned token | `token_not_allowlisted` / `token_template_mismatch` |
| `swap-neg-kron-state` | a KRON payer token with `is_minter = 1`, `id_type` 0 or 1, or without the owner's P2PK input | `token_borrow_enabled` / `token_owner_scheme` |
| `swap-neg-kron-merchant` | a KRON token as the merchant asset (`kcc20` profile) | `token_not_allowlisted` |
| `swap-neg-alias-ask` | section 8.6 attack X (merchant is the ask's maker) | `route_aliasing` |
| `swap-neg-alias-bid` | token variant (merchant is the bid's maker, slot equals the merchant output) | `route_aliasing` |
| `swap-neg-order-later` | an order input at index `k` or later | `route_aliasing` |
| `swap-neg-unknown` | a covenant that is not a pinned KOB template, as an input; a conditional or if-done order | `unknown_order_template`; `route_unsupported` |
| `swap-neg-extra-output` | an unclaimed output that is neither merchant, payer token change nor payer KAS change | `invalid_payload` (`invalid_kaspa_exact_payment_output`) |
| `swap-neg-borrow` | a borrow-enabled payer, custody or merchant token UTXO | `token_borrow_enabled` |
| `swap-neg-payasset` | `route.payAsset` outside `payAssets`; a payer token of another token | `pay_asset_not_accepted` |
| `swap-neg-underpay` / `swap-neg-overpay` | merchant output below or above `amount` | `underpayment` / `overpayment` |
| `swap-neg-conflict` | a named order outpoint already spent | `order_conflict`, retryable, `details.orders` |
| `swap-neg-swr1` | facilitator substitutes an order outpoint after signing | payer signature fails |
| `swap-neg-swr2` | merchant output redirected after signing | validation fails |
| `swap-neg-hashtype` | a correctly signed payment (the engine accepts it) whose payer token leader witness, delegator witness or P2PK funding signature uses SIGHASH_NONE `0x02`, SIGHASH_SINGLE `0x04` or an ANYONECANPAY form `0x81`, `0x82`, `0x84`; a trailing byte that is no hash type (`0x00`, `0x03`, `0x05`, `0xff`); KCC-20, KaspaCom-template and KRON (presence input) pay assets | `token_owner_scheme` |
| `swap-neg-commit` | commitment without `route`, or with another `payAsset` | `invalid_authorization` |

The conformance cases of intent mode and invoices are listed in sections 17.11 and 18.8.

## 22. Open questions for the binding's maintainers

1. **Scope.** Is an optional routed extension of `exact` acceptable, or should it be a separate
   scheme name? It reuses `exact` because the merchant outcome is an exact amount at a fixed
   output, only the payer's side differs.
2. **Critical flag.** Is `extra.route.critical = true` an acceptable way to mark a critical `extra`
   member under the binding's rule, or do you want a general `criticalExtensions` list?
3. **KAS as pay asset.** For a merchant that wants token B, the payer may pay KAS through an
   ask-only route (KOB scenario SW2): the reference defines `payAssets` entry `{ "asset": "KAS" }`
   and `route.payAsset = "KAS"` (sections 3 and 5). Is that the shape you want?
4. **Payer surplus outputs.** Section 8.4 leaves a payer-owned surplus output of B optional. Do you
   prefer it mandatory, forbidden, or left to builders?
5. **Non-KOB orders.** Would you want the abstract requirement of section 4 in the binding proper,
   with the KOB orders as a registered instantiation?
6. **Conflict detail shape.** `extensions.kaspa.details.orders` is an array of outpoints; would you
   prefer a top-level response member so non-Kaspa clients can read it without the `kaspa`
   extension?
7. **Retry semantics.** Section 10 requires fresh authorization for a re-signed attempt and
   mutually exclusive attempts (input reuse) or a permanent-absence proof. Is that consistent with
   your `maxPaymentRetries` intent?
8. **Intent mode as part of `exact`.** In intent mode the payer's transaction pays an intent, not the
   merchant, and the merchant is paid by a later transaction the facilitator builds. Is that still `exact`
   (the merchant outcome is exact), or a separate scheme? Should `router` name a program by artifact id, as
   here, or by the template hashes of its actors?
9. **Deadline in the covenant.** The KOB router carries the deadline in the intent and lets anyone
   expire the intent from it on (section 17.7); a short window remains between the deadline and the
   expiry's acceptance. Should the extension require (I4) of every intent program, and should a
   facilitator report the late payment when a third party executes in that window?
10. **Invoices in the binding.** Is an invoice object (section 18) in scope for the binding or for a
   companion document? Should a QR code carry the plain `https` URL, as here, or a scheme such as
   `x402:`? Should the invoice id be bound to the facilitator (today it is the content hash only)?

## 23. References

- `spec/kaspa-exact-v2.md`, `spec/kaspa-x402-v1.md`, `spec/facilitator-profile.md`, `spec/errors.md`
  (elldeeone/kaspa-x402, v1.0.0-rc.1).
- `x402-kcc20-profile.md` (companion proposal).
- KOB `docs/spec/order-types.md`, `docs/spec/matcher.md` (positional outputs, custody, strays),
  `docs/spec/kob1-payload.md` (`X402` record, pinned order template hashes),
  `contracts/v2/KobBid.sil`, `contracts/v2/KobAsk.sil` (positional rules).
- KOB `crates/kob-tests/tests/kob_gap_tests.rs` (`gap_swap_and_pay`, `gap_swap_resign_required`).
- KOB `crates/kob-x402`: `wire.rs` (`RoutePayload`, `BINDING_SWAP`), `common.rs`
  (`PayloadCommit.route_pay_asset`), `error.rs` (`Diag::OrderConflict`, `Diag::RouteAliasing`).
- KIP-9 (extended mass), KIP-20 (covenant ids).
- KOB `docs/argent.md` (the router), `crates/kob-protocol/src/router.rs` and `src/build/intent.rs` (intent builders),
  `crates/kob-x402/src/intent.rs`, `src/invoice.rs`, `src/client/intent.rs`, `crates/kob-executor/src/x402/facilitator/`
  (`intent.rs`, `invoice.rs`).
