# Kaspa x402 exact: KCC-20 token profile (`kcc20`)

Status: Draft proposal. Addressed to [elldeeone/kaspa-x402](https://github.com/elldeeone/kaspa-x402)
(v1.0.0-rc.1, binding `kaspa-exact-v2`, checked against commit `040b1ec`). Proposed amendment to
`spec/kaspa-exact-v2.md`; nothing here changes the `standard-native` or `additive` profiles.
Reference implementation: KOB, crate `kob-x402` (`crates/kob-x402/src/{wire,common,policy,error,safe_tx,token}.rs`),
which is the source of the field names and digests below. Companion proposal:
`x402-swap-and-pay.md` (payer pays in one token, merchant receives KAS or another token, through
order-book orders in one transaction). That document reuses the authorization defined here.

The key words MUST, MUST NOT, REQUIRED, SHOULD, SHOULD NOT and MAY are used as in RFC 2119.

## 1. Summary

`exact` settles native KAS only (`kaspa-exact-v2`, "The binding settles native KAS only";
"fungible tokens or non-native assets" are listed under "Current alpha exclusions"). This proposal adds
one profile, `kcc20`, that settles a fixed amount of a KCC-20 token, and it changes two things the
KAS profiles could leave implicit:

| # | Change | Where |
|---|---|---|
| 1 | Every token requirement carries an explicit custody class, `extra.token.custody` (`unconditional` or `issuer-controlled`). Acceptance of a token whose issuer can freeze or seize is never presented as final custody. | Sections 3, 4, 9 |
| 2 | The authorization is a digest committed inside the transaction payload (`kob-x402-payload-commitment-v1`), not a Schnorr signature by one P2PK input. Every input authorizer's SIGHASH_ALL covers it, whatever the owner scheme. | Section 6 |
| 3 | The safe-json projection and the transaction id rules cover version-1 transactions with output covenant bindings. | Section 7 |
| 4 | A token payment is verified against a pinned token program (template hash) and pinned extension commitment; borrow-enabled token UTXOs are refused. | Sections 5, 8 |

## 2. Motivation

The two points below were raised publicly by the KOB author on kas-smiths.org (topic 15, post #20);
the binding's author agreed to revisit them when tokens are introduced (post #24).

### 2.1 Accepting a token payment is not unconditional custody

A KAS output is final in the sense that no third party holds a rule over it once the transaction is
accepted. A token output is not, in general. A token program can contain entries by which an issuer
or admin key freezes a holder's balance, seizes it, burns it, mints against it, or gates every
transfer on an allowlist. A merchant that accepts such a token pays out a resource against a balance
that may be frozen or taken away after `accepted` or `confirmed`. Calling that payment settled, in
the same words as a KAS payment, misstates what the merchant holds.

The proposal therefore makes the custody class part of the signed requirement (it is inside
`paymentRequirementsHash`, so the payer's authorization commits to it), lets a server call a token
`unconditional` only after a recorded review of the exact program, and has the facilitator, the client
and the response repeat the class, so the risk cannot be dropped on the way to the merchant's
accounting (section 4).

### 2.2 The authorization digest assumes one P2PK signer

The binding's request authorization is a Schnorr signature over a digest, by "the public key proven
by that authoritative standard P2PK funding input" at `authorization.inputIndex`. That holds for
`standard-native`, where the only inputs are payer P2PK funding inputs. It does not hold for a token
transfer:

- the spend authorization of a token input is a script witness selected by the token's owner scheme
  (Schnorr P2PK, hashed Schnorr key, hashed ECDSA key, P2SH authority input, or covenant-id
  ownership), not necessarily a P2PK key that appears in the input's UTXO;
- a transfer can spend several token inputs with different owners, and a leader input authorizes all
  token outputs while delegator inputs authorize themselves;
- a payer may have no plain P2PK KAS input at all (fees paid from the token UTXOs' KAS value), so
  there is no "funding input at `inputIndex`" to name.

A separate signature by one key would also be a second signing operation, disjoint from the
transaction signatures, that only some of the authorizers produce. The proposal instead binds the
request to the transaction itself, in a way every authorizer's signature already covers
(section 6).

## 3. Identifiers and requirement

The `kcc20` profile applies to `scheme: "exact"` on the recognized networks. Amounts are token
base units (the smallest unit of the token; `decimals` is display metadata here). Any positive
base-unit amount can be requested: when the payer buys the token on the KOB books (swap-and-pay,
intents), the orders trade base units too (no lots) and quote prices in sompi per WHOLE token
(`price` per `scale = 10^decimals` base units, an order state field; `order-types.md`, "Amounts,
prices and rounding"); the order's `scale`, not this offer's `decimals`, is what its covenant uses.

```json
{
  "scheme": "exact",
  "network": "kaspa:testnet-10",
  "amount": "250",
  "asset": "<KCC-20 covenant id, 64 lowercase hex>",
  "payTo": "kaspatest:<Schnorr P2PK address of the merchant>",
  "maxTimeoutSeconds": 60,
  "extra": {
    "binding": "kaspa-exact-v2",
    "profile": "kcc20",
    "finality": "accepted",
    "transactionEncoding": "kaspa-sdk-safe-json-v2.0.0",
    "payToScriptPublicKey": "0000<P2PK script of payTo>",
    "token": {
      "family": "kcc20",
      "templateHash": "<64 hex>",
      "extensionCommitment": "<64 hex>",
      "custody": "unconditional",
      "carrier": "100000000",
      "tokenScriptPublicKey": "0000<P2SH script of the merchant token output>",
      "decimals": 8,
      "ticker": "EXAMPLE"
    }
  }
}
```

| Field | Rule |
|---|---|
| `amount` | MUST be a canonical positive uint64 decimal string in token base units, and at most `9223372036854775807` (token amounts are signed 64-bit script integers). It is the entire merchant gain in tokens. |
| `asset` | MUST be the KIP-20 covenant id of the token, 64 lowercase hex characters, not all zero. It is the token identity together with `templateHash` and `extensionCommitment`. |
| `payTo` | MUST be a Schnorr P2PK address for `network`. Its x-only key is the owner of the merchant token output. Any other address type MUST be rejected. |
| `extra.profile` | MUST equal `kcc20`. |
| `extra.payToScriptPublicKey` | Unchanged: the canonical serialized script public key derived independently from `payTo` (a P2PK script). It is used to derive the owner key; it is not the script of the token output. |
| `extra.token.family` | MUST equal `kcc20` in this proposal. Other token families need their own profile. |
| `extra.token.templateHash` | MUST be the SilverScript template hash (BLAKE3 over the program prefix and suffix, without the state) of the one KCC-20 program the server accepts for `asset`. |
| `extra.token.extensionCommitment` | MUST be the 32-byte extension commitment every token UTXO of this payment carries (the KCC-20 state field). Fungibility exists only among UTXOs with equal commitments. |
| `extra.token.custody` | MUST equal `unconditional` or `issuer-controlled` (section 4). |
| `extra.token.carrier` | MUST be a canonical positive uint64 sompi string: the exact KAS value of the merchant token output. A server SHOULD choose it as an ordinary covenant-UTXO carrier so the merchant's later spend is not dominated by storage mass (section 13.3). The KOB reference accepts 1 to 2 KAS (policy bounds `min_carrier_sompi` and `max_carrier_sompi`; the payer funds the carrier, so the ceiling protects the payer). |
| `extra.token.tokenScriptPublicKey` | MUST be the serialized script public key of the merchant token output: `P2SH(prefix ‖ state ‖ suffix)` of the pinned program with `state = { amount = amount, owner = x-only key of payTo, owner_scheme = 0x00, borrow_scheme = 0x00, borrow_guard = 32 zero bytes, extension_commitment = extensionCommitment }`. A verifier MUST recompute it and reject disagreement. |
| `extra.token.decimals`, `extra.token.ticker` | Optional, display only. They MUST NOT be used to identify or to select a token, and a client MUST NOT trust them (two tokens can share a ticker). |
| `extra` other | As in the binding: unknown fields are ignored and still covered by the requirements hash. `extra.finality` follows the binding; section 9.1 defines `confirmed` numerically for this profile. |

`extra` MUST NOT contain the additive-profile head or challenge fields.

## 4. Custody classes

### 4.1 Definition

| Class | Meaning |
|---|---|
| `unconditional` | The token program was reviewed and has no admin, freeze, seize, mint or burn path that can reach a holder balance. Once the payment is at the required finality, the merchant's units cannot be frozen, taken, destroyed or diluted by any party that does not hold the merchant's owner key. |
| `issuer-controlled` | The program, or an extension its state commits to, can reach a holder balance through an issuer or admin path, or the review was not done. Accepting the token is an explicit merchant risk. |

A server decides `unconditional` only when all of the following hold, and it MUST record the
review that establishes them:

1. **Reviewed program.** A person or process the operator names has read the complete program
   source of the pinned template and found no entry, branch or extension hook by which a party other
   than the owner-scheme witness can spend, reduce, freeze, redirect or destroy a holder's balance
   UTXO, or make its spend depend on a third-party approval, and no mint or burn entry. Owner
   schemes that let a third party authorize a spend (covenant-id ownership, P2SH authority input)
   count as reaching a balance unless the third party is the holder itself. The borrow path counts
   too: it lets a third party respend a borrow-enabled UTXO (section 5.3), which is why the profile
   also requires `borrow_scheme = 0` on every UTXO.
2. **Template hash pinned.** `extra.token.templateHash` equals the reviewed program's template hash
   and the verifier requires it of every token input and output (section 8). Review of one program
   says nothing about another program that reuses the covenant id (section 13.1).
3. **Extension commitment pinned.** `extra.token.extensionCommitment` equals the reviewed value and
   the verifier requires it of every token UTXO. If the program interprets the commitment (an
   extension that carries issuer logic), the review covers that extension too; a different commitment
   is a different asset.

Anything else is `issuer-controlled`, including a token that was never reviewed. A server MUST NOT
label a token `unconditional` on the issuer's own claim, and MUST switch the class to
`issuer-controlled` (or stop offering the token) when a later review, a program change or a
covenant-lineage change invalidates any item above. `unconditional` is the server's attestation, not
a fact the chain proves; its value is that it is explicit, pinned and signed over.

The KOB reference derives the class from its token registry (`registry/tokens.json`). Its default
allowlist holds the tokens that are `listed` and `verified` and whose program is on the registry's
strict list (`reviewed`). The class is `unconditional` only when the program's registry `capabilities`
are empty; any capability (freeze, seize, blacklist, mint authority, public mint, burn) makes it
`issuer-controlled`, and a merchant then needs `allow_issuer_controlled`. Any other token is served
only if the operator adds it explicitly. Only KCC-20 tokens are merchant assets of this profile: a KRON
token (a swap-and-pay pay asset, `x402-swap-and-pay.md`) is refused (`token_not_allowlisted`).

### 4.2 What each party MUST and SHOULD do

Facilitator (or a server verifying directly):

- MUST serve a token only if it is in the operator's token allowlist, with `asset`, `templateHash`,
  `extensionCommitment` and `custody` all equal to the allowlist entry. A disagreement in any field
  fails as `invalid_payment_requirements` (section 11).
- MUST NOT verify or settle an `issuer-controlled` payment unless the operator enabled
  issuer-controlled tokens explicitly (default off).
- MUST echo the class of the settled payment in the response (section 10) and MUST NOT describe an
  `issuer-controlled` settlement as final custody or as equivalent to a KAS settlement.

Resource server:

- MUST advertise the class the allowlist records.
- SHOULD, for `issuer-controlled`, require `finality: "confirmed"` or a stricter policy, SHOULD treat
  the receipt as a credit exposure to the issuer, and SHOULD NOT release irreversible goods against
  it without that acknowledgement.

Client (payer wallet, agent):

- MUST reject a `kcc20` entry whose `custody` is missing or is not one of the two values; it MUST NOT
  treat a missing class as `unconditional`.
- MUST show the class (and the token's covenant id, not only its ticker) before signing, and MAY be
  configured to refuse `issuer-controlled` entries entirely.
- SHOULD prefer an `unconditional` entry when the server offers both.

### 4.3 Response caveat

The settlement response for a `kcc20` payment MUST carry `extensions.kaspa.custody` with the
requirement's class (section 10). For `issuer-controlled` the acceptance evidence covers "the
transaction is in the accepted UTXO set", not "the merchant's units are safe from the issuer", and
any human-readable receipt MUST say so.

## 5. Canonical transaction

The reference construction is a version-1 native transaction (covenant bindings exist only in
version 1):

```text
input[0..t)   = payer KCC-20 token UTXOs (t >= 1); input[0] is the leader
input[t..)    = optional payer standard Schnorr P2PK KAS funding inputs

output[paymentOutputIndex] = merchant token output   (value == carrier, spk == tokenScriptPublicKey,
                                                      covenant { authorizingInput = leader, covenantId = asset })
optional output            = payer token change      (covenant { authorizingInput = leader, covenantId = asset })
optional output            = payer KAS change        (P2PK, no covenant)
```

Builders SHOULD place the merchant token output at index 0. `paymentOutputIndex` in the payload is
authoritative for the verifier, which MUST check that it names the unique merchant token output.

### 5.1 Rules

The transaction MUST:

- use version 1, the native subnetwork, zero gas, zero lock time, and a KOB1 payload carrying the
  authorization commitment of section 6 (and nothing else except optional notes);
- spend only (a) token UTXOs of covenant id `asset` and (b) standard Schnorr P2PK KAS UTXOs with no
  covenant; every other input kind is rejected;
- make every token input carry: the covenant id `asset`; a P2SH script public key whose
  redeem script (the last signature-script push) equals `prefix ‖ state ‖ suffix` of the pinned
  program, checked against the trusted UTXO script and the pinned template hash; `owner_scheme =
  0x00` (Schnorr P2PK owner); `borrow_scheme = 0x00`; the pinned `extension_commitment`; a positive
  amount;
- designate the **leader** as the token input with the lowest input index (the KCC-20 program
  designates the first input carrying the covenant id as leader; it authorizes all continuations),
  and give every output with a covenant binding `authorizingInput` equal to the leader's index and
  `covenantId` equal to `asset`;
- contain exactly one merchant token output: value exactly `extra.token.carrier`, script exactly
  `extra.token.tokenScriptPublicKey` (which fixes the amount, the owner key `payTo`, and the
  borrow-disabled state), covenant binding as above;
- contain at most one payer token change output, present exactly when the token inputs sum to more
  than `amount`: its script is recomputed from `state = { amount = Σ inputs − amount, owner = an
  owner key of one of the payer's token inputs, owner_scheme 0x00, borrow_scheme 0x00, borrow_guard
  0, extension_commitment = pinned }`; a zero-amount output is invalid;
- contain at most one payer KAS change output, paying the P2PK script of the key of a verified
  payer input (a funding input or a token input's owner), without a covenant binding;
- contain no other output;
- conserve tokens: Σ token input amounts = merchant amount + token change amount (the token
  program enforces this too; the verifier recomputes it);
- have a non-negative fee within client and verifier policy and no less than the node's relay floor
  (`100 sompi per gram × max(compute mass, 2 × size)`; a verifier is never stricter than the node);
- use the version-1 `computeBudget` input field, commit the storage mass required by the active
  consensus rules, and satisfy isolation, populated-transaction, mass and txscript validation
  (section 8);
- carry no ANYONECANPAY, NONE or SINGLE signature: every payer signature uses SIGHASH_ALL.

The client pays `amount` tokens, `carrier` KAS (it becomes the merchant token output's value), and
the fee. The merchant receives exactly `amount` tokens on exactly `carrier` KAS.

### 5.2 Token program limits

The reference KCC-20 program bounds the number of token inputs and outputs per transfer at compile
time (3/3 for the upstream reference, which is also what KOB issues; the 8/8 variant is a prototype; other programs
exist). A token payment needs
one input and up to two outputs (merchant and change); a client SHOULD consolidate before paying
when the payment needs more inputs than the program allows. Each token input carries the whole
program in its signature script (about 2.9 kB for the reference 3/3 program, about 6.4 kB for the
8/8 prototype); a payment's size, and so its fee floor, is dominated by that.

### 5.3 Why borrow-enabled token UTXOs are refused

A KCC-20 UTXO whose `borrow_scheme` is not `0x00` can be respent through the program's borrow path
by a party other than the owner (any party that increases the amount by more than a threshold, or
the holder of a borrow guard), which creates a successor UTXO. In the x402 flow the payer signs a
transaction and hands it over before it is broadcast. If any input, or the merchant output that is
later the merchant's balance, can be borrowed, then between signing and broadcast a third party can
spend that outpoint: the signed payment becomes invalid (a griefing race the payer cannot prevent),
or, for the merchant output, the merchant's balance can be moved off the pinned outpoint. Requiring
`borrow_scheme = 0x00` on every payer token input, on the merchant output and on the change output
removes that path: the only party that can spend those UTXOs is their owner. It also completes the
`unconditional` argument of section 4.1, since a borrow-enabled UTXO has a spender other than the
holder.

## 6. Payload and authorization

### 6.1 Payload object

`PaymentPayload.payload` keeps the binding's shape (`type: "exact-transaction"`, `transaction`,
`transactionEncoding`, `paymentOutputIndex`, `requestHash`, `payerAddress?`) with:

- `profile: "kcc20"`;
- `authorization` replaced by the payload commitment below (no `inputIndex`, no `signature`).

```json
{
  "x402Version": 2,
  "accepted": { "...": "the selected kcc20 requirements" },
  "payload": {
    "type": "exact-transaction",
    "profile": "kcc20",
    "payerAddress": "kaspatest:...",
    "transaction": "<signed bounded safe transaction JSON>",
    "transactionEncoding": "kaspa-sdk-safe-json-v2.0.0",
    "paymentOutputIndex": 0,
    "requestHash": "<normalized request hash>",
    "authorization": {
      "version": "kob-x402-payload-commitment-v1",
      "expiresAt": "2026-10-01T12:00:00.000Z",
      "digest": "<32-byte digest>"
    }
  }
}
```

`payerAddress` is receipt metadata only, as in the binding.

### 6.2 The digest

Construct this object, using lowercase hex for every hex field:

```json
{
  "scope": "kob-x402-payload-commitment-v1",
  "network": "kaspa:testnet-10",
  "profile": "kcc20",
  "route": null,
  "asset": "<covenant id>",
  "amount": "250",
  "payTo": "kaspatest:...",
  "payToScriptPublicKey": "<lowercase serialized script of payTo>",
  "paymentOutputIndex": 0,
  "paymentRequirementsHash": "<lowercase hash>",
  "requestHash": "<lowercase hash>",
  "expiresAt": "2026-10-01T12:00:00.000Z"
}
```

`route` is `null` for a direct token payment; the swap-and-pay extension sets it to
`{ "binding": "kob-swap-v1", "payAsset": "<covenant id>" }` (companion proposal), and its intent mode to
`{ "binding": "kob-intent-v1", "payAsset": "<covenant id or KAS>", "actor": "<router actor>" }` (companion
proposal, section 17.4: the transaction that carries the commitment is then the intent's creation, and
`paymentOutputIndex` names the intent output). For a payment of an invoice (companion proposal, section 18),
`requestHash` is the invoice id, so the commitment also binds the invoice's reference and expiry. The digest is
SHA-256 over the UTF-8 bytes of the object under the binding's canonical JSON (keys sorted in
ascending UTF-16 code-unit order, arrays in order, compact, integers only). `paymentRequirementsHash`
and `requestHash` are defined exactly as in the binding. The digest omits `challengeId` (there is
none), `inputIndex` and `transactionId`.

### 6.3 Embedding in the transaction payload

The transaction payload MUST be a KOB1 payload (payload version `0x02`, see KOB
`docs/spec/kob1-payload.md`) that contains exactly one `X402` record (type `0x81`, optional class)
whose value is the 32-byte digest, and no other record except optional `NOTE` records (type `0x82`).
The minimal payload is 40 bytes:

```text
4b 4f 42 31   magic "KOB1"
02            version
81 20 00      record X402, length 32 (u16 little-endian)
<32 bytes>    the digest
```

The legacy `X402:<hex>` text payload MUST NOT be accepted as a commitment. A payment transaction
MUST NOT carry `ORDER` records (nor the reserved record type `0x02`, which no KOB1 decoder accepts). A verifier bounds the payload (KOB reference: 512 bytes)
before decoding it.

### 6.4 Why every authorizer covers it

The Kaspa signature hash of every input, for every hash type, includes the hash of the transaction
payload when the payload is not empty (`payload_hash` in the consensus signature-hash function). A
SIGHASH_ALL signature additionally commits to every input outpoint and to every output including
its covenant binding. Consequently:

- each payer signature (owner witnesses of token inputs, funding-input signatures) is valid only for
  a transaction with exactly this payload, hence exactly this digest;
- the digest fixes the requirements (through their hash), the request, the payment output index and
  the expiry;
- the transaction id is therefore bound transitively: the payer's signatures fix the whole
  transaction, which fixes the payload, which fixes the digest.

No party, including the facilitator, can lift a payer's signed transaction and present it for a
different request, different requirements or a different expiry without invalidating every payer
signature, and none can remove the commitment. There is no separate signature to produce, store or
forget.

### 6.5 Why the transaction id is not in the digest

The digest sits inside the payload, and the payload is part of the transaction id preimage: the
version-1 id is a BLAKE3 hash over `payloadDigest ‖ restDigest` where `payloadDigest` is a hash of
the payload bytes. A digest containing the transaction id would be a fixed point of the id function.
The transitive binding above makes the id unnecessary: the verifier recomputes the id from the
canonical transaction as the binding requires ("A separate client-authoritative transaction id is
forbidden") and reports it in the response.

### 6.6 Generalization to owner schemes other than P2PK

The property needed is: at least one authorizer of the transaction is a party that (a) is bound to
the payment, and (b) produces a signature whose hash covers the payload and every output. The
commitment provides it whenever the signature is a Kaspa signature over the transaction (Schnorr
or ECDSA, SIGHASH_ALL), regardless of where the public key comes from:

| KCC-20 owner scheme | Authorizer | Covers the commitment? | This revision |
|---|---|---|---|
| `0x00` P2PK Schnorr | Schnorr signature in the witness | Yes | Accepted |
| `0x01` hashed Schnorr key | public key and signature in the witness | Yes | Reserved; MAY be accepted by a verifier that decodes the witness |
| `0x02` hashed ECDSA key | public key and signature in the witness | Yes | Reserved, as above |
| `0x03` P2SH authority | the script of another input; covered only if that script checks a SIGHASH_ALL signature | Depends on that script | Rejected unless the authority script is pinned and reviewed to require one |
| `0x04` covenant id | presence of a covenant input | No (the owning program decides; it may require no signature) | Rejected |

A profile revision that accepts more schemes MUST keep two invariants: the verifier identifies for
every input which signature authorizes it and that its hash type is SIGHASH_ALL (`token_owner_scheme`
otherwise), and at least one authorizer that is not a covenant script is present. The digest object
and the embedding do not change. Because the commitment carries no signer identity, the same object
authorizes multi-owner payments (several token inputs with different owners, each signing) and
payments with no P2PK funding input.

## 7. Safe-json and transaction id rules for version 1 with covenants

### 7.1 Projection

`kaspa-sdk-safe-json-v2.0.0` already defines version-1 inputs (`computeBudget`, `sigOpCount: 0`).
The binding requires every output's `covenant` to be `null`. This proposal changes that rule to:

- `covenant` MUST be `null` for `standard-native` and `additive`;
- for `kcc20` (and routed payments), `covenant` is `null` for an output without a binding, and
  otherwise an object with exactly two members:

```json
{ "authorizingInput": 0, "covenantId": "<64 lowercase hex>" }
```

`authorizingInput` is a uint16 JSON integer (an input index), `covenantId` the 32-byte covenant id.
An input's embedded `utxo` hint MAY carry `covenantId` (lowercase hex) for a covenant input; like
every hint it MUST match the trusted chain data. A verifier MUST reject a `covenant` object on a
version-0 transaction and any additional member of the object. `storageMass` remains mandatory.

### 7.2 Transaction id

The id is computed from the normalized consensus fields exactly as the binding defines for version 1
("Version 1 transaction id"). The binding's serialization section says that a version-1 output
appends a covenant-presence byte, "followed by its binding only when present"; this profile relies
on the exact encoding, which matches the Rusty Kaspa consensus code: after the output's value,
script version and script bytes, an output with a covenant appends `0x01`, `authorizingInput` as
uint16 little endian, then the 32 covenant-id bytes; an output without a covenant appends `0x00`.
The payload is the KOB1 commitment payload, so `payloadDigest` is over those exact bytes. Signature scripts,
compute budgets and storage mass are excluded from the id, as in the binding. The identifier MUST
equal what the consensus library computes, and a convenience `id` in the artifact MUST equal it.

## 8. Verification

The order follows the binding's "Verification" list; steps marked (new) are additions for this
profile.

1. Validate x402 version, scheme, network, `asset` (covenant id shape), `amount` (canonical, at most
   `2^63 − 1`), timeout, binding, profile `kcc20`, and every field of `extra`, including a complete
   `extra.token`. Require that `payload.accepted` equals the server's offered requirements
   (canonical JSON equality) and `payload.profile` equals `kcc20`.
2. Re-derive `payToScriptPublicKey` from `payTo` and reject disagreement; require `payTo` to be a
   Schnorr P2PK address.
3. (new) Look up `asset` in the operator's token allowlist. Require `templateHash`,
   `extensionCommitment` and `custody` to equal the entry, `custody = issuer-controlled` only if
   enabled, and `carrier` within the operator's bounds (`carrier_mismatch` otherwise). Recompute `tokenScriptPublicKey` from the
   pinned program and the merchant state and reject disagreement.
4. Enforce artifact byte, input, output, signature-script, payload and metadata limits before
   expensive parsing or node calls.
5. Canonically deserialize the transaction (version 1) and recompute its identifier.
6. Resolve every input UTXO from a trusted node or chain adapter (amount, script, covenant id) and
   reject any disagreement with the artifact's hints, including a hinted covenant id.
7. (new) Classify every input: a token input (covenant id `asset`) whose signature-script redeem
   script is `prefix ‖ state ‖ suffix` of the pinned program (P2SH of the redeem script equals the
   trusted script, template hash equals the pin), with a decoded state satisfying section 5.1
   (`owner_scheme` 0, `borrow_scheme` 0, pinned extension, positive amount); or a P2PK KAS funding
   input. Reject anything else, including a second token or covenant.
8. Verify that every payer signature's hash type is SIGHASH_ALL, then every payer Schnorr signature
   through the script engine under the active rules. The hash type is read from the signature
   script itself, per owner scheme, before the engine runs: the engine accepts a correctly signed
   NONE, SINGLE or ANYONECANPAY transaction, so only this check refuses it (`token_owner_scheme`).
   Where the signature sits: a P2PK funding input carries one 65-byte push (`sig64 ‖ type`); a
   token input's witness is the third push from the end of its signature script (arrays, witness,
   dispatch tag, redeem script for the leader; witness, tag, redeem script for a delegator). The
   leader's witness is `0x00 ‖ proof` (the owner path), a delegator's the proof alone. The proof
   per owner scheme: `0x00` `sig64 ‖ type` (65 bytes); `0x01` `pubkey32 ‖ sig64 ‖ type` (97);
   `0x02` `pubkey33 ‖ sig64 ‖ type` (98); `0x03` one byte, the index of the authority input, whose
   script decides the hash type (refused, section 6.6); `0x04` empty. A malformed script or proof is
   `invalid_kaspa_exact_signature`.
9. Enforce the canonical shape and the exact merchant gain of section 5.1: unique merchant token
   output at `paymentOutputIndex`, exact value and script, covenant bindings, token conservation,
   payer change recomputed, no other output.
10. Recompute conservation, fee, compute mass, storage mass (must equal the committed value), script
    units and compute commitments; apply configured bounds and the relay floor.
11. Validate the transaction in isolation and with the populated UTXO context using current Rusty
    Kaspa behavior, including execution of every token program input with the enforced budget.
12. (new) Recompute the payload commitment digest from the offer, the request hash and
    `authorization.expiresAt`; require it to equal `authorization.digest` and the single `X402`
    record of the transaction payload; check `authorization.version`.
13. Check expiry (section 9.3), then enforce request binding, transaction replay and
    payment-identifier policy.

Implementation status: every step of this list is enforced by the reference verifier, including the
hash type of the owner witnesses and funding signatures (step 8, the SIGHASH_ALL rule of section
5.1; `crates/kob-x402/src/sighash.rs`). The reference verifier accepts owner scheme `0x00` only, so
schemes `0x01` to `0x04` are `token_owner_scheme` refusals before step 8; the hash-type parser
nevertheless decodes all five (section 6.6). The same check guards the swap-and-pay payer signatures
and the intent creation (`x402-swap-and-pay.md`, sections 8.1 and 17.5); the native `exact` verifier
accepts only the canonical 65-byte SIGHASH_ALL push per input (`invalid_kaspa_exact_signature`). The
builders sign only with SIGHASH_ALL and refuse a wallet signature of another type.

Nothing in the artifact is authoritative merely because it is present: not ids, amounts, scripts,
masses, fees, decoded states or finality claims. Facilitator `/verify` and `/settle` keep the
binding's mandatory, never-inferred `requestHash`.

## 9. Settlement, finality, replay, expiry

### 9.1 Lifecycle and finality

The lifecycle is the binding's: verify with trusted chain facts, durably consume replay evidence,
broadcast the exact verified transaction, observe finality, respond with the recomputed id.
Ambiguous outcomes stay consumed. Two profile-specific points:

- **Broadcast path.** The transaction MUST be submitted through a node interface that preserves
  version-1 fields (compute budgets, covenant bindings, storage mass). An interface that drops or
  rewrites them (a REST gateway that re-serializes) MUST NOT be used.
- **Accepted UTXO-set semantics.** `accepted` means the merchant token output
  (`transactionId : paymentOutputIndex`, script `tokenScriptPublicKey`, value `carrier`) is in the
  virtual UTXO set of the trusted node. Mempool presence is not acceptance. `confirmed` means
  `accepted` and the virtual DAA score minus the DAA score of the accepting block is at least the
  operator's confirmation depth; the reference depth is 100 DAA (about 10 seconds at the current
  block rate). The effective finality is the stronger of the offer and the server's policy, as in
  the binding.

A reorg or node disagreement after a response is delivered is an ambiguous settlement: consumed
evidence stays consumed and the payment is reconciled, never released, as in the binding.

### 9.2 Replay and idempotency

- The `payment-identifier` extension is required, bound to the request fingerprint and the profile
  `kcc20`, exactly as in the binding, and to the transaction it first arrived with: another
  transaction under a bound id is answered `kaspa_payment_identifier_conflict` (never with the
  outcome of the bound transaction) until the bound one has failed. Payers draw ids at random.
- A transaction id and every outpoint the transaction spends (all token and funding inputs) are
  consumable at most once per trust domain, so two different transactions cannot spend one payer
  UTXO for two requests.
- The digest does not need its own ledger entry: it is fixed by the request hash and the
  requirements, and re-signing the same digest into a different transaction requires new payer
  signatures and consumes different outpoints.

### 9.3 Expiry

`authorization.expiresAt` follows the binding's expiry rule: it MUST parse as a millisecond UTC
timestamp strictly after the verifier's `now` and no later than `now + maxTimeoutSeconds`; a client
SHOULD set it to `clientNow + maxTimeoutSeconds`. Expiry is re-evaluated after any awaited
verification and before creating a new settlement. As in the binding, it does not invalidate
recovery of an exactly matching attempt whose transaction was already durably accepted.

### 9.4 Expiry does not expire the transaction; revocation

Kaspa transactions have no upper time bound (a lock time is only a lower bound). `expiresAt` limits
when a facilitator will accept and settle the payment; the signed transaction stays valid on chain
for as long as its inputs stay unspent, and anyone who holds it can broadcast it later. The
payer's revocation is a self-spend: spend any one input of the signed transaction in a different
transaction (for example move a token input to an owner-controlled UTXO, or spend a funding input).
Because every token input and funding input is spendable only by its owner (borrow is refused), the
payer can always do this.

A client that discloses a signed `kcc20` payment SHOULD revoke it by self-spend once it can no
longer be settled (authorization expired without acceptance evidence). This also gives the client
the binding's "authoritative permanent-absence proof": a trusted, confirmed different spend of a
persisted input outpoint proves that the disclosed transaction can never be accepted, and only then
does the binding's retry rule allow a replacement payment.

## 10. SettlementResponse

Success extends the binding's `extensions.kaspa` (its fields `binding`, `profile`,
`paymentOutputIndex`, `finality`, `transactionEncoding` are unchanged):

```json
{
  "success": true,
  "transaction": "<recomputed transaction id>",
  "network": "kaspa:testnet-10",
  "payer": "kaspatest:...",
  "amount": "250",
  "extensions": {
    "kaspa": {
      "binding": "kaspa-exact-v2",
      "profile": "kcc20",
      "paymentOutputIndex": 0,
      "finality": "accepted",
      "transactionEncoding": "kaspa-sdk-safe-json-v2.0.0",
      "custody": "unconditional"
    }
  }
}
```

`amount` is the accepted requirement amount in token base units (not sompi); `transaction` is the
recomputed id; `extensions.kaspa.custody` MUST equal the requirement's class (section 4.3). A
failure carries `errorReason` (a public reason from `errors.md`) and MAY carry
`extensions.kaspa = { diagnostic, retryable, message, details? }` with a diagnostic from section 11.

## 11. Diagnostics added

Public reasons are the binding's closed set (`errors.md`); the local diagnostic travels in
`extensions.kaspa.diagnostic` and is not the compatibility contract. The rows below are added for
this profile (the reference implementation also uses the binding's existing diagnostics, for
example `invalid_kaspa_exact_utxo`, `invalid_kaspa_exact_fee`, `invalid_kaspa_exact_mass`).

| Diagnostic | Public reason | Raised when |
|---|---|---|
| `token_not_allowlisted` | `invalid_payment_requirements` | `asset` is not in the operator's token allowlist, or the offer names a token the server does not serve. |
| `token_custody_policy` | `invalid_payment_requirements` | The offered `custody` differs from the allowlist entry, or the class is `issuer-controlled` and the operator did not enable it. |
| `token_template_mismatch` | `invalid_payment_requirements` for the offer's pins; `invalid_payload` for an input or output | `templateHash` or `extensionCommitment` differs from the pin; a token input or output does not carry the pinned program or extension. |
| `token_borrow_enabled` | `invalid_payload` | A token input, the merchant output or the change output has `borrow_scheme` other than `0x00`. |
| `token_owner_scheme` | `invalid_payload` | A token input has an owner scheme other than `0x00`, or a signature's hash type is not SIGHASH_ALL. |
| `token_conservation` | `invalid_payload` | Token inputs do not sum to merchant amount plus change, or a zero-amount output exists. |
| `carrier_mismatch` | `invalid_payload` | The merchant token output's value differs from `extra.token.carrier`. |
| `underpayment` | `invalid_payload` | The merchant token output carries fewer than `amount` units or is missing. |
| `overpayment` | `invalid_payload` | It carries more than `amount` units, or a second output pays the merchant. |
| `invalid_authorization` | `invalid_payload` | The commitment digest, version or embedded `X402` record is wrong or missing. |
| `expired_authorization` | `invalid_transaction_state` | `expiresAt` is not after the verifier's now. |
| `authorization_exceeds_max_timeout` | `invalid_payload` | `expiresAt` is later than `now + maxTimeoutSeconds`. |

## 12. Schema amendments

`schemas/kaspa-payment-payload.schema.json`:

- `profile` enum: add `"kcc20"`.
- `authorization`: replace the single object by a `oneOf`:
  - the existing object (`version` const `kaspa-x402-exact-request-authorization-v1`,
    `inputIndex`, `expiresAt`, `digest`, `signature`);
  - `{ version: const "kob-x402-payload-commitment-v1", expiresAt, digest }`, required, with
    `additionalProperties: false` (so `inputIndex` and `signature` are forbidden).
- Conditional: when `profile` is `kcc20`, `authorization.version` MUST be the payload commitment and
  `challengeId` MUST be absent; for `standard-native` and `additive` it MUST be the signed form.

`schemas/kaspa-requirements-extra.schema.json`:

- `profile` enum: add `"kcc20"`; when `kcc20`, require `token` and forbid the additive-profile
  fields.
- `token`: object, `additionalProperties: true`, required `family` (const `kcc20`), `templateHash`
  and `extensionCommitment` (hash32), `custody` (enum), `carrier` (canonical uint64 string, positive),
  `tokenScriptPublicKey` (`^0000(?:[0-9a-fA-F]{2})+$`); optional `decimals` (integer 0 to 18) and
  `ticker` (string).

`schemas/payment-required.schema.json`: `asset` becomes `KAS` for the KAS profiles and the covenant-id
pattern (`^[0-9a-f]{64}$`, not all zero) when `extra.profile` is `kcc20`; `amount` for `kcc20` is
additionally capped at `9223372036854775807`.

`schemas/settlement-response.schema.json`: for a `kcc20` success, `extensions.kaspa.custody` (enum)
is required.

Transaction projection (`kaspa-exact-v2.md`, "Rules"): the sentence "every output includes
`covenant`, which MUST be `null` in this binding" becomes the rule of section 7.1.

`GET /supported` (`facilitator-profile.md`): a facilitator that serves the profile lists `"kcc20"` in
`profiles` and adds `extra.tokens`, an array of `{ asset, templateHash, extensionCommitment, custody,
ticker?, decimals? }` for the tokens it accepts. It MUST omit `kcc20` unless its verifier, allowlist
and chain adapter are configured and healthy.

## 13. Security considerations

### 13.1 Lookalike tokens

A KIP-20 covenant id identifies a lineage; it does not by itself pin the program that runs at each
UTXO of the lineage. A program that permits successor outputs with a different script lets a later
UTXO of the same covenant id carry different code, and unrelated tokens can share a ticker, a name
and decimals. The profile therefore identifies a token by three values checked on every UTXO: the
covenant id, the template hash (recomputed from the redeem script, never read from a field), and the
extension commitment. Every input's redeem script is compared to the trusted UTXO script and to the
pinned template; every output the verifier recomputes is derived from the pinned program. Display
metadata (`ticker`, `decimals`) MUST NOT drive selection or acceptance, and a client SHOULD display
the covenant id (for example `TICKER (abcd…1234)`).

### 13.2 Issuer powers

`unconditional` is only as good as the review and the pins. Risks a server accepts by serving
`issuer-controlled` tokens: freeze or seizure after acceptance, burn or dilution, gated transfers,
and a program whose extension logic changes behavior without changing the covenant id. Section 4 is
the mitigation: the risk is explicit in the signed requirement, in the facilitator's policy and in
the response. It does not remove the risk and does not prevent a payer from paying with a token that
an issuer freezes before broadcast (the payment then fails; no funds move).

### 13.3 Carrier and storage mass

The merchant output's KAS value (`carrier`) is fixed by the server, so a payer cannot hand the
merchant a token UTXO too small to spend economically. KIP-9 storage mass charges an output of value
`v` up to `C / v` grams (`C = 10^12`) before the input-side credit. The relay floor of a rusty-kaspa
v2.1.0 node is `100 sompi per gram × max(compute mass, 2 × size)` and does not price storage mass, but
consensus bounds storage mass by the block limit (500,000 grams) and block templates rank a
transaction by its fee over its normalized mass including storage, so a small output makes a
transaction invalid (about 0.02 KAS and below) or lowers its priority under contention. A server
SHOULD choose a carrier of at least 1 KAS. Payer change outputs are the payer's cost; a builder MUST
NOT let a change output push the transaction over the storage limit and SHOULD avoid tiny change (fold
it into the fee within the verifier's fee bound). The reference builders pay the relay floor by
default and a storage-inclusive fee on request (priority).

### 13.4 Verifier cost

A token input carries a signature script of several kB and every token input is executed by the
script engine. A verifier MUST bound inputs, outputs, signature-script bytes and payload bytes before
running the engine, MUST run the engine with the enforced compute budget, and SHOULD rate-limit
per source and per merchant before doing chain lookups. The reference defaults are 32 inputs, 16
outputs, 32 KiB of signature script per input, a 256 KiB transaction artifact and a 512 byte
payload.

### 13.5 Unsigned transaction fields

Signature hashes do not cover signature scripts or, for version 1, compute budgets and the committed
storage mass. A third party can alter them without invalidating payer signatures (a larger budget
raises the fee floor above the paid fee; another witness encoding changes nothing that consensus
accepts as valid). The verifier therefore recomputes masses, checks the fee floor and runs the
engine, and the facilitator broadcasts exactly the verified bytes, never a rebuilt transaction.

### 13.6 Wallet signing limits (informative)

The profile needs, from a signer, Schnorr signatures under SIGHASH_ALL over the signature hashes of
the payer's inputs (the owner witness of each token input and the signature of each funding
input); assembling the token-input signature scripts (arguments plus the whole program) is separable
from signing. To the best of the authors' knowledge at the time of writing, only two wallet
interfaces sign covenant (P2SH) inputs in a transaction they did not fully build themselves: KasWare
through `signPskt`, and Kaspire through ordered signing arguments. Other wallets cannot sign these
transactions. The KOB SDK works around this by returning per-input signing requests (input index,
hash type, key) that any signer able to produce a Schnorr signature over a supplied signature hash can
fulfil, and by assembling the final signature scripts itself. The profile does not depend on any
wallet API; this paragraph is informative and will age.

## 14. Test vectors

The suite below is not published yet. What exists today: engine-validated positive and negative scenarios in
`crates/kob-x402/tests/kcc20.rs` (`kcc20-neg-hashtype`: `a_token_owner_witness_must_use_sighash_all`,
`a_delegator_owner_witness_must_use_sighash_all`, `a_funding_input_signature_must_use_sighash_all`,
`the_hash_type_rule_holds_for_every_token_program`) and `crates/kob-x402/tests/sighash.rs`
(`kcc20-neg-hashtype-schemes`), and golden requests and results of the payer and verifier functions
(`kcc20` offers, commitment digest, payment, preflight, the five `preflight.kcc20.hashtype.*` negatives, revocation) in
`crates/kob-x402/vectors/golden/x402-payments.json`. The suite will be language-independent JSON in the
style of the binding's `vectors/exact/`, produced by the reference implementation and checked by engine
validation of every transaction. Each case will carry the
selected requirements, both canonical JSON preimages, their SHA-256 results, the payload bytes, the
transaction in safe-json form, the trusted UTXOs, the recomputed transaction id and, for negatives,
the expected diagnostic and public reason. Placeholders below are filled at publication.

| Id | Case | Expected |
|---|---|---|
| `kcc20-pos-1` | one token input, no change, no funding input | valid; requirements hash `<TBD>`, digest `<TBD>`, txid `<TBD>` |
| `kcc20-pos-2` | token change and KAS change, one funding input | valid; `<TBD>` |
| `kcc20-pos-3` | two token inputs with different owners, multi-signature | valid; `<TBD>` |
| `kcc20-neg-template` | input with another program under the same covenant id | `token_template_mismatch` |
| `kcc20-neg-ext` | wrong extension commitment | `token_template_mismatch` |
| `kcc20-neg-borrow` | borrow-enabled payer input; borrow-enabled merchant output | `token_borrow_enabled` |
| `kcc20-neg-owner` | a token input with an owner scheme other than `0x00` (`0x01` to `0x04`) | `token_owner_scheme` |
| `kcc20-neg-hashtype` | a correctly signed payment (the engine accepts it) whose leader witness, delegator witness or P2PK funding signature uses SIGHASH_NONE `0x02`, SIGHASH_SINGLE `0x04`, or the ANYONECANPAY forms `0x81`, `0x82`, `0x84`; a trailing byte that is no hash type (`0x00`, `0x03`, `0x05`, `0xff`); on every token program | `token_owner_scheme` |
| `kcc20-neg-hashtype-schemes` | the hash-type parser per owner scheme: `0x00` (65-byte proof), `0x01` (97), `0x02` (98), each with every hash type, as leader and as delegator; `0x03` authority (refused); `0x04` (no signature); malformed proofs | `token_owner_scheme` / `invalid_kaspa_exact_signature` (malformed) |
| `kcc20-neg-carrier` | merchant output value differs from `carrier` | `carrier_mismatch` |
| `kcc20-neg-amount` | merchant units below or above `amount` | `underpayment`, `overpayment` |
| `kcc20-neg-extra-output` | an additional output, a second merchant output | `overpayment` or `invalid_kaspa_exact_payment_output` |
| `kcc20-neg-leader` | output binding names a non-leader input | `invalid_kaspa_exact_transaction` (engine) |
| `kcc20-neg-commit` | digest mismatch; payload missing; two `X402` records; legacy text payload | `invalid_authorization` |
| `kcc20-neg-expiry` | expired; later than `maxTimeoutSeconds` | `expired_authorization`; `authorization_exceeds_max_timeout` |
| `kcc20-neg-custody` | offered class differs from allowlist; issuer-controlled disabled | `token_custody_policy` |
| `kcc20-neg-accepted` | `accepted` differs from the offer | `invalid_kaspa_x402_accepted` |
| `kcc20-neg-replay` | same txid or same input outpoint for a second request | `invalid_kaspa_exact_replay` |
| `kcc20-intent-k2t` | KAS locked in a `KasToToken` intent (companion proposal, section 17); the execution's merchant token output | exact units at `tokenScriptPublicKey`, value `carrier`, borrow disabled |
| `kcc20-intent-swap` | token A locked in a `TokenSwap` intent paying this profile's token B | as `kcc20-intent-k2t` |
| `kcc20-invoice` | a `kcc20` entry of an invoice paid with the invoice id as `requestHash`; a second payment | `paid`; `invoice_paid` (not broadcast) |

## 15. Open questions for the binding's maintainers

1. **Profile or binding revision.** Should `kcc20` be a third profile of `kaspa-exact-v2`, or belong to
   a `kaspa-exact-v3` that also revises the safe-json `covenant` rule? Until you decide, KOB emits
   `extra.binding = kaspa-exact-v2` and distinguishes the profile by `extra.profile`.
2. **Asset identifier.** Is a bare covenant id acceptable as `asset`, or do you want a CAIP-19-style
   qualified identifier (for example `kaspa:mainnet/kcc20:<id>`)?
3. **Commitment container.** The commitment travels in a KOB1 `X402` record because KOB indexers
   already parse that container. A binding-neutral alternative (a distinct magic plus 32 bytes) is
   equally workable for KOB, which would then accept both. Which do you prefer?
4. **One authorization mechanism.** The payload commitment also works for `standard-native` (and
   removes a signing operation that a transaction-only wallet cannot perform). Would you consider
   it as an alternative authorization for the KAS profile, so that the binding has one mechanism
   instead of two?
5. **Custody taxonomy and registry.** Are two classes enough? Who should maintain the reviewed-program
   list, and should the binding define a registry format so that `custody: unconditional` is
   comparable across operators? In the shipped KOB registry (`registry/tokens.json`) no KCC-20 program is
   `reviewed` and no token is `listed`, so the default allowlist is empty.
6. **`confirmed` depth.** The binding leaves the numeric depth to adapters. Would a documented
   default (100 DAA here) be welcome for token profiles?
7. **Owner schemes.** Should a later revision define the P2SH-authority and hashed-key owner schemes
   in the normative table of section 6.6, or keep them reserved?
8. **Response field.** Is `extensions.kaspa.custody` the right place for the class, or should it
   also be surfaced in a receipt extension?
9. **Commitment outside the paying transaction.** In the intent mode of the companion proposal the
   commitment sits in the creation, a transaction that pays an intent, not the merchant; the merchant
   output is in a later transaction the facilitator builds. Is the commitment the right authorization
   there, or would you want the execution to carry a reference to it as well (KOB executions carry none
   today)?

## 16. References

- `spec/kaspa-exact-v2.md`, `spec/kaspa-x402-v1.md`, `spec/facilitator-profile.md`, `spec/errors.md`,
  `schemas/kaspa-payment-payload.schema.json`, `schemas/kaspa-requirements-extra.schema.json`
  (elldeeone/kaspa-x402, v1.0.0-rc.1).
- KOB `docs/spec/kob1-payload.md` (the `X402` record), `docs/spec/matcher.md`, `docs/spec/order-types.md`.
- KOB `crates/kob-x402`: `wire.rs` (`AUTH_VERSION_PAYLOAD`, `Profile::Kcc20`), `common.rs`
  (`payload_commit_object`, `payload_commit_digest`, `commitment_payload`, `check_payload_commitment`,
  `check_expiry`), `policy.rs` (`Custody`, `TokenAllowlist`), `error.rs` (`Diag`, `Reason`),
  `safe_tx.rs` (covenant projection).
- KCC-20 reference program (argent-lang/kcc20-reference, master `c8a0871`, pull request 1 merged; unaudited
  at the time of writing), standalone build (`contracts/kcc20/KCC20Ref.sil`): state `{ amount, owner, owner_scheme, borrow_scheme, borrow_guard,
  extension_commitment }`, leader and delegator entries, borrow path.
- KIP-9 (extended mass), KIP-10 (introspection), KIP-20 (covenant ids).
- kas-smiths.org, topic 15, posts #20 and #24.
