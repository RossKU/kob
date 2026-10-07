# KOB as an Argent app

KOB is an Argent app on the outside and hand-written SilverScript on the inside. This document
says what is published, how another Argent app imports it, and what the KOB router on top of it
does. Nothing here is audited. Problems met while doing this, each with a minimal reproduction,
are in [argent-feedback.md](argent-feedback.md). Compiler pin and patches: [../argent/UPSTREAM.md](../argent/UPSTREAM.md).

| Layer | What | Where |
|---|---|---|
| Contracts | Hand-written SilverScript: the protocol v2 orders (KobAsk, KobBid, KobCondAsk, KobCondBid, KobIfdAsk, KobIfdBid, and the pair orders KobPair, KobCondPair, KobIfdPair). Per-order values live in the state span, so each kind has one template. | `contracts/v2/*.sil`, `contracts/artifacts/*.json` |
| Argent shell | The same contracts published as the Argent app `KOBOrders` (actor handles, state layouts, entries), the KRON limit orders as `KOBOrdersKron`, plus the 8/8 token as `KOBToken`. | `contracts/argent/KOBOrders/`, `contracts/argent/KOBOrdersKron/`, `contracts/argent/KOBToken/` |
| Composition | KOB's own Argent app: payment intents and swap-and-pay. | `contracts/argent/kob_router.ag`, `contracts/argent/router/` |

Matching, batches and wallet flows stay in Rust (`kob-protocol`); Argent is used where other
apps compose with KOB.

## Published artifacts

Reference build, produced by `scripts/build-contracts.sh` from the committed sources. Pin the
artifact id you reviewed; do not pin a file name or a branch.

| App | Artifact id (`id` field of `artifact.json`) |
|---|---|
| `KOBOrders` | `173c7c979dd747316aacbf6e93ec04890a4256f043be94d148b943e05b71f127` |
| `KOBOrdersKron` | `867d24a6c06fb8a38a30941a7f5a79d8dda05c4bee4b01f3dab2ba68c394442f` |
| `KOBToken` | `dae9bf3f9c4761e8d37aeafe53650dd7b05d65c35dcb38bf97f69a64941006a0` |
| `KobRouter` | `7047fc189b70b6bdbd6fb361751f6c185af57bea68f982d833ecf78a8e628dda` |

Actor-type handles (the template hash of the hand-written program: blake3 over prefix and suffix
around the state span, the same value silverc reports as `template_hash`):

| Actor | Handle | State span | Program |
|---|---|---|---|
| `KOBOrders::KobAsk` | `070bb3b2425cc800c02e2e9465241a8b94abdce93e712fd3baf19474b63b887c` | 1+276 | 1,684 B |
| `KOBOrders::KobBid` | `b995661f8b17c7c558b85975e361c26b00a2d57214cf631083a464b835e54fa3` | 1+285 | 1,516 B |
| `KOBOrders::KobCondAsk` | `d9ed37c5aa03d5c62619659f86c6a1e08ab4ba9a1092d144b2160935331e6adb` | 1+363 | 3,530 B |
| `KOBOrders::KobCondBid` | `00c5f808562dc4ead4121f992de13959896042dbe717ad84c130f463acd36a1f` | 1+381 | 3,433 B |
| `KOBOrders::KobIfdAsk` | `a2f5cb8da7cc60da43c625f2cffd747294df952800b2545fa9f5eee5d3c53d79` | 1+594 | 4,870 B |
| `KOBOrders::KobIfdBid` | `6aaaa5447c3d9d27294e4e49c8eb988f4347ec0af7543c2d7b94cabb19be031e` | 1+585 | 3,789 B |
| `KOBOrders::KobPair` | `c95c92344f08c42992699b0e467cf32fe573dda9b879250438d60a13cab4a9c6` | 1+447 | 2,960 B |
| `KOBOrders::KobCondPair` | `7b8f1a9e957a9def15fd835fb2e47331ccfcc856978f6408f8a3c7a519a6f06e` | 1+543 | 6,284 B |
| `KOBOrders::KobIfdPair` | `4a432afb1bd314e43a5497df414f9e8121866dfa59a59bad6f1a9d141e06669a` | 1+909 | 8,789 B |
| `KOBOrdersKron::KobAskKron` | `f7274b79b081fbbf05d14b006359883c144304adb0ec0c6f9b8741feaef8f76d` | 1+243 | 1,487 B |
| `KOBOrdersKron::KobBidKron` | `6ec1a3dd4a287b73295a08db5f75fedcac4966539d793e9d1a659711ad888efc` | 1+252 | 1,343 B |
| `KOBToken::KCC20` (8 in / 8 out) | `40fef59a59bd76991f4d4e2101d1e3e34860997b89fe7714532637cec482a9d7` | 1+112 | 6,820 B |

The test `tools/sil2argent/tests/published.rs` fails if the ids and handles above, or the router
actors and entries listed under "Fill shapes", disagree with the committed artifacts.

Two things about the reference build:

- **Build constants, final handles.** `KobCondAsk` and `KobCondBid` embed the KobAsk/KobBid templates
  (their trigger evidence, read from a fill in the same transaction); `KobIfdBid` and
  `KobIfdAsk` embed the exit template and the evidence template of their stop entry; `KobCondPair` embeds
  the `KobAsk` and `KobBid` templates of both families and `KobPair` (its trigger evidence), `KobIfdPair` its exit
  `KobCondPair` and the same evidence templates. None of them depends on a network or a genesis, so every
  handle above is final at build time and the same on every network (`contracts/deploy/testnet-10`
  only records them, see `contracts/README.md`).
- **The interface is not the implementation.** `contracts/argent/KOBOrders.ag` declares state
  layouts and entries with placeholder bodies. `tools/sil2argent` compiles it with argentc, refuses
  the result if the hand-written contract differs in state field names, order or Sil types, in
  entry names or argument types, or if the skeleton has compiler-owned context, hidden entry
  parameters or witness recipes, then replaces every generated contract by the silverc artifact of
  the hand-written source and recomputes the template receipts, handles, interface fingerprints
  and the artifact id. What is reviewed and shipped is the `.sil` and its silverc artifact; the
  Argent artifact is derived from them and checked by CI.

`KOBToken` is a separate app on purpose: an actor inside a multi-actor Argent app gets
compiler-owned template fields in its state, which would change the token program. As its own
single-actor app, argentc output is byte-identical (modulo comments) to
`contracts/kcc20/variants/KCC20Ref_8x8.sil`, and its handle is the silverc template of
`contracts/artifacts/KCC20Ref_8x8.json`. Argent's `actor_type_handle` (the stable template view an
importer uses) does not change this: in a multi-actor app (checked on argentc `b312ded`) the token
gets a template context field (state 1 + 145 instead of 1 + 112 bytes) and a 9,864 B program instead
of 6,820 B, and its handle correctly describes THAT program (template `3d08058a...`), not the
deployed `KCC20Ref_8x8` (`40fef59a...`) that token UTXOs carry and orders pin
(`docs/argent-feedback.md`, item 8).

## Importing KOBOrders from another Argent app

Two ways to name a KOB order kind in `observes`. Both compile-checked examples live in
`contracts/argent/examples/` (`scripts/build-argent.sh` builds them).

### Closed ICC (recommended)

```
import "./KOBOrders/artifact.json" id "173c7c979dd747316aacbf6e93ec04890a4256f043be94d148b943e05b71f127";

actor PriceGate owns PriceGateState {
    entry spend(cov_id ask_covid, int n, sig s)
    observes book by ask_covid {
        inputs  { ask:  KOBOrders::KobAsk, }
        outputs { next: KOBOrders::KobAsk, }
    }
    emits none {
        require(n > 0);
        require(n < book.inputs.ask.amountLeft); // state layout comes from the artifact
        ...
        // a partial fill continues with amountLeft - n (every other field unchanged); a refund or
        // cancel has no continuation.
        KobAskState next_a = KobAskState { maker: book.inputs.ask.maker, ..., amountLeft: book.inputs.ask.amountLeft - n };
        require book.outputs become { next <- KOBOrders::KobAsk(next_a), };
    }
}
```

The complete example is `contracts/argent/examples/closed_icc_gate.ag` (compiled by
`scripts/build-argent.sh`; `crates/kob-tests/tests/argent_import_pin_tests.rs` builds it against the
genuine and a forged artifact).

1. Copy `contracts/argent/KOBOrders/artifact.json` (the reference build, final on every network) into your repository and
   **pin its artifact id in the import** (`import "./KOBOrders/artifact.json" id "<id>";`). The
   patched argentc refuses an artifact whose id differs and an artifact import without a pin: a
   forged artifact with the same app name and ABI is self-consistent too (`check_consistency` only
   proves the id is the content hash), so the pin is what ties your contract to the reviewed orders.
   In CI run `sil2argent verify <artifact.json> --id <id> KobAsk=<silverc artifact> ...`:
   it re-derives the id, every embedded contract, every handle and the fingerprints against the
   silverc artifacts, without argentc, and refuses any other id. KOB's own build does the same
   (`scripts/build-argent.sh` stops when KOBOrders changes until the router's pin is updated).
2. Build with an argentc that supports artifact-backed imports: `argent/build-argentc.sh` builds
   the pinned upstream commit plus our patch 0002 (`argent/UPSTREAM.md`). Stock upstream argentc
   reads a `.json` import as Argent source and fails (artifact-backed imports are Argent's own open
   item "Support artifact-backed app dependencies"; patch 0002 is prepared as an upstream PR).
3. argentc does not compile KOBOrders. It loads and consistency-checks the artifact, checks the
   pinned id, turns its states and exported actors into a declaration-only module (so
   `KOBOrders::KobAsk` and `KobAskState` resolve as for a source import), links the handle as a
   constant (`gen__kob_orders__kob_ask_template_const`), and records the dependency (`app`,
   `artifact_id`) and the imported interface fingerprints in your artifact. Entries that
   observe an order take hidden witnesses: the handle's prefix and suffix lengths (`KobAsk`:
   1 and 1,407). A keeper reads them from the artifact and supplies them (the harness does this by
   hand: `encode_entry_sig_script`; feedback A2).
4. **Trust.** With a source import argentc compiles the exporter and derives the handle itself; with an
   artifact import the handle comes from the artifact, and Argent does not check a linked handle against
   the exporter's receipt (the interface fingerprint excludes the handle). Without the pin, whoever
   supplies the artifact controls which template your contract accepts. That is why the id is pinned
   (`argent_import_pin_tests.rs` shows both sides: the source import derives the skeleton's own
   handle, the artifact import takes whatever the artifact says, and only the pin refuses a forged
   artifact; feedback item 4).

The router is a real closed-ICC importer of KOBOrders and KOBOrdersKron (`KOBOrdersKron` is a separate app with the
KRON twins of the limit orders, so the published KOBOrders id stays as it is). It observes the token programs by
open ICC (next section).

### Open ICC (no artifact import)

```
state OpenPriceGateState {
    pubkey owner;
    int max_price;
    actor_type<KobAskState> ask_type;   // value: the published KobAsk handle
}
...
observes book by ask_covid { inputs { ask: self.ask_type, } outputs { next: self.ask_type, } }
```

The handle is data in your contract's state: put the KobAsk handle from the table above into
`ask_type` when you create the instance. You must declare `KobAskState` yourself, field for field
in the order of the state span (`contracts/argent/KOBOrders.ag`); on chain the check is structural
(template hash and layout), but in an earlier test `argent-runtime` matched an open handle to an actor by
state **name**, so use the published name (not re-checked on the current pin, feedback A4). Open ICC
needs only patch 0001 (rusty-kaspa v2.1.0), no artifact-backed import, and lets one instance point at
any template you choose, which also means the importer's users must check the handle value they are
asked to fund.

Evidence: both examples are compiled by `scripts/build-argent.sh`. The closed-ICC path is exercised
end to end against the v2 orders by the router harness. The open-ICC example is compile-checked only.

Open or closed, the observed order is a covenant input of the same transaction and **validates
itself** (payout, continuation, escrow pinning). Your app checks only what its owner cares about.
An observed group has an exact shape (here: one order input, one order output); a fill that sells
the order out has another shape and needs its own entry.

## The router

`contracts/argent/kob_router.ag` (app `KobRouter`, generated `.sil` in `contracts/argent/router/sil/`;
the `.ag` itself is written by `tools/router-gen`, see "The router generator") has three payment
intents, the two that sell a token A in a KCC-20 and a KRON variant. Each is one covenant UTXO that any keeper or
facilitator can spend; the payer signs nothing at execution time and can always `cancel` (SIGHASH_ALL). Every intent
has a `deadline` (see "Deadline" below): from it on anyone may `expire` the intent, which returns everything to the payer.

| Intent | Payer locks | Merchant receives | Payer's worst case |
|---|---|---|---|
| `KasToToken` | KAS | exactly `amount` of token B, bought from 1-3 KobAsks | pays at most `max_pay` at the asks' quotes (summed over the sweep) plus `max_extra` |
| `TokenToKas` | token A (owner = the intent's covenant id) | at least `merchant_kas` KAS, raised by selling into 1-3 KobBids | sells at most `max_sell` units (summed over the sweep) |
| `TokenSwap` | token A (as above) | exactly `amount_b` of token B: A into 1-2 KobBids, the KAS pays 1-2 KobAsks of B | sells at most `max_sell_a` units of A (summed over the sweep) |
| `TokenToKasKron`, `TokenSwapKron` | a KRON token A (id_type 2, owner = the intent's id, not a minter) | as `TokenToKas` / `TokenSwap`; A is sold into KobBidKrons, token B is KCC-20 | as above; the lock exceeds the bound (a KRON output holds at least one unit) |

**Token programs: open ICC.** The orders are linked by closed ICC (their published handles). The tokens are not: each
intent's state names every token twice, by covenant id and by program handle (`actor_type<KCC20State> token_type`;
the swaps `token_a_type`, `token_b_type`; the KRON intents `actor_type<KronTokenState>` for token A). The router
reads every observed token input and pins every token output under the handle in the state, so one actor serves every
KCC-20 program of the 112-byte state (the 3/3 reference program, the 8/8 program KOB issues, the 4/5 and 16/16
variants) and both KRON programs (2,433 B and 2,732 B: raw bytecode, not Argent apps, so open ICC is the only way to
observe them; `KronTokenState` is declared in the router, `owner id_type amount is_minter`). The payer chooses the handle
at creation and nothing can change it afterwards; a verifier recomputes it from its allowlist like every other term
(the x402 facilitator takes the program of the offer's `templateHash`), so a merchant is never delivered tokens of
another program than its offer names, and a keeper cannot pick a program. The programs' own slot limits bound the
shapes they run: the 3/3 program has no room for a three-bid sell (four token outputs), the builders refuse it before
anything is locked. A KRON token is paid with, never received (x402 `kcc20` merchant assets are KCC-20), so no
`KasToToken` actor buys KRON. KaspaCom's KCC20 0.2.5 program stays out of the intents while it is pending review
(`kob_protocol::router::intent_program`).

**Lock pin.** A token intent's lock is the one token-A UTXO owned by the intent's covenant id (KCC-20 owner_scheme
0x04, KRON id_type 2), but anyone can create another such UTXO: a token transfer may name any covenant id as owner.
Every observed group has an exact shape (one token input), so before 2026-10-06 an entry handed such a stand-in as its
lock accepted it: a 1-unit dust and an `expire` after the deadline (or a keeper's own units in a fill) ended the intent
while the real lock, owned by a covenant id no transaction can carry again, stayed frozen for good. The token intents'
state now pins the lock: `lock_amount` (its exact base units) and, for a KCC-20 token A, `lock_extension` (its
extension commitment), both set by the creation from the lock it makes (`IntentState::lock_pin`; the builder refuses a
state that pins another lock, and mixed extension commitments). Every entry that spends the lock (the fill, `expire`,
`cancel`) requires both (`router_lock_pin` in the harness: X1-X5 rejected, ablations A-X1 / A-X5). What remains is a
stand-in of exactly `lock_amount` units with the same commitment: it returns the lock's full value to the payer at the
stand-in maker's expense. Intents created from the previous router templates (testnet-10 only, short-lived by their
deadline) keep the old script: their payer's `cancel` and anyone's `expire` still validate on chain, but the stand-in
attack still works on them, and today's builders do not build for their templates (a template this build does not pin
is unsupported: `docs/spec/template-retirement.md`).

**B pin.** A KCC-20 token is (covenant id, program, extension commitment): units of the same covenant id with another
commitment are another token, which only the token's issuer can create (in its genesis). A KobAsk had no extension
field (its custody was any token UTXO of its covenant id that it owned), so until 2026-10-07 an intent that buys token B
took it from whatever class the asks' escrows held and delivered that class to the merchant. A KobAsk now names the
commitment of its custody (`extensionCommitment`, which can be any commitment of the covenant id), and the intents that
buy token B (`KasToToken`, `TokenSwap`, `TokenSwapKron`) carry `b_extension`, the extension commitment of the offer's token B
(set by the payer's client, recomputed by the verifier from the merchant asset's allowlist entry); every escrow of an
observed ask must carry it and the merchant's delivery is pinned to it (`IntentState::b_extension`; the execution
builder refuses escrows of another commitment). `crates/kob-protocol/tests/intent_builders.rs`,
`an_intent_takes_token_b_only_of_its_extension_commitment`: every such actor on every program pair, token B of another
commitment of the same covenant id is refused at the intent input (an ask takes such an escrow only when its own state
names that commitment). Intents created from the previous templates keep their old script and their `cancel` /
`expire`; today's builders do not build for them.

The spread between what an order releases and what the payer authorised (crossing spread, tips,
auction decay) belongs to the keeper. For `TokenToKas` and `TokenSwap` the intent's own KAS carrier
is the keeper's; for `KasToToken` the payer's `max_extra` bounds what carriers and the network fee
may take.

### Deadline

Every intent's state ends with `temporal deadline` (unix ms): the payer's authorization expiry
(x402 intent mode sets it to `authorization.expiresAt`, never later than an invoice's expiry). From
the deadline on, **anyone** may spend the intent with `expire()` (no signature, CLTV
`tx.time >= deadline`; a node accepts the transaction once its past median time reached the
deadline):

| Intent | out j | out j+1 |
|---|---|---|
| `KasToToken` | the intent's KAS to the payer (P2PK), less at most `EXPIRE_MAX_FEE` (0.1 KAS) | - |
| `TokenToKas`, `TokenSwap` | the intent's KAS to the payer, less at most `EXPIRE_MAX_FEE` | the locked tokens, whole and with their whole KAS carrier, to the payer's key (borrow disabled, same extension); input j + 1 must be those tokens |

The payer's `cancel` works at any time, before or after the deadline; a token intent's cancel must spend the locked tokens at input j + 1, as the one token input of the locked token, read under the intent's handle like every observed token: owned by the intent's id (KCC-20 owner_scheme 0x04, KRON id_type 2). The cancel ends the intent covenant, and tokens owned by its id could never move afterwards, so the router refuses a cancel that leaves them behind or spends other tokens of that token in their place; where they go (the one token output of that token) is the payer's signed choice. **What the deadline does
not do:** refuse a late execution by the clock. A covenant can prove a lower time bound (CLTV) but
never an upper one (a transaction valid at t stays valid later), so until the intent is spent an
execution after the deadline still validates. What ends an abandoned intent is the expiry
transaction: the x402 facilitator submits it itself once the deadline passed without a settlement
(`docs/spec/x402-swap-and-pay.md`, section 17.7), and after it confirms nobody can execute the intent;
the facilitator also knows how the intent ended (its own expiry, the payer's cancel, or an execution).
The expiry costs 2.7-6.6 kB (0.006-0.013 KAS) for a `KasToToken` intent and, measured by
`crates/kob-protocol/tests/intent_builders.rs` (2026-10-06, with the lock pin), 11.4-17.2 kB for a token intent on the
8/8 program, 7.6-13.5 kB on the 3/3 program and 6.3-9.6 kB for a KRON intent (it carries the token program); it adds 82-87 B to every
`KasToToken` script and about 840 B to every token intent script (the observed token group of the expiry). The
cancel's observed lock adds about 380 B more to every token intent script.

### Fill shapes

Argent has no observed ranges yet (an `observes` group has an exact shape; argentc rejects
`Actor[1..=3]` in an observed group), so every shape a keeper may use needs its own entry. The
redeem script of a covenant UTXO carries every entry of its actor, so **each shape is its own actor
(one template)**: the shape's entry, `expire` and `cancel`, nothing else. The fill shape is known when the
payer creates the intent, so the payer locks its funds in the actor of the shape it wants
(`KasToToken_buy2_out`, `TokenSwap_swap`, ...) and every intent UTXO stays small. The actors of one
intent share the state and the rules. Per order there are two shapes:

- **rest**: the order is filled partially and continues. A KobAsk continues with `amountLeft - n` and
  its custody UTXO holds the rest (GTC asks only); a KobBid continues byte-identically.
- **out**: the order ends. A KobAsk is **sold out** (the whole custody is taken; any time-in-force,
  so a FOK or IOC ask that is filled completely also ends this way); a KobBid terminates (its
  buying power is exhausted, or it is IOC/FOK). No covenant output is left for the order.

A sweep over several orders is `out, .., out, rest` or `out, .., out, out`: only the last order of
a sweep may rest (what a best-price-first matcher produces). Orders are named in the entry
arguments in transaction order; the asks' escrows are matched to them by owner.

| Actor (`KasToToken_<shape>`) | Entry (visible arguments) | Redeem |
|---|---|---|
| `_buy` | `buy(ask)`: one ask rests | 2,847 B |
| `_buy_out` | `buy_out(ask)`: one ask sold out | 2,612 B |
| `_buy2` | `buy2(ask1, ask2)`: ask 1 sold out, ask 2 rests | 4,747 B |
| `_buy2_out` | `buy2_out(ask1, ask2)` | 4,488 B |
| `_buy3` | `buy3(ask1, ask2, ask3)` | 6,634 B |
| `_buy3_out` | `buy3_out(ask1, ask2, ask3)` | 6,384 B |

| Actor (`TokenToKas_<shape>`) | Entry | Redeem |
|---|---|---|
| `_sell` / `_sell_out` | `sell(bid, n)` / `sell_out(bid, n)` | 4,077 B / 4,065 B |
| `_sell2` / `_sell2_out` | `sell2(bid1, bid2, n1, n2)` / `sell2_out(..)` | 5,654 B / 5,642 B |
| `_sell3` / `_sell3_out` | `sell3(bid1, bid2, bid3, n1, n2, n3)` / `sell3_out(..)` (not on the 3/3 program) | 7,291 B / 7,279 B |

| Actor (`TokenSwap_<shape>`) | Entry | Redeem |
|---|---|---|
| `_swap` | `swap(bid, ask, n_a)`: bid and ask rest | 6,627 B |
| `_swap_bid_out` | `swap_bid_out(bid, ask, n_a)`: the bid ends, the ask rests | 6,608 B |
| `_swap_ask_out` | `swap_ask_out(bid, ask, n_a)`: the bid rests, the ask is sold out | 6,349 B |
| `_swap_out` | `swap_out(bid, ask, n_a)` | 6,337 B |
| `_swap2` | `swap2(bid1, bid2, ask1, ask2, n_a1, n_a2)` | 10,143 B |
| `_swap2_out` | `swap2_out(bid1, bid2, ask1, ask2, n_a1, n_a2)` | 9,856 B |

| Actor (`TokenToKasKron_<shape>`) | Entry | Redeem |
|---|---|---|
| `_sell` / `_sell_out` | `sell(bid, n)` / `sell_out(bid, n)` into KobBidKrons | 3,491 B / 3,479 B |
| `_sell2` / `_sell2_out` | `sell2(bid1, bid2, n1, n2)` / `sell2_out(..)` | 4,954 B / 4,942 B |
| `_sell3` / `_sell3_out` | `sell3(bid1, bid2, bid3, n1, n2, n3)` / `sell3_out(..)` | 6,432 B / 6,413 B |

| Actor (`TokenSwapKron_<shape>`) | Entry | Redeem |
|---|---|---|
| `_swap` / `_swap_bid_out` | `swap(bid, ask, n_a)` / `swap_bid_out(..)`: KobBidKron(s) of the KRON token A, KobAsk(s) of B | 6,018 B / 6,006 B |
| `_swap_ask_out` / `_swap_out` | `swap_ask_out(bid, ask, n_a)` / `swap_out(..)` | 5,778 B / 5,766 B |
| `_swap2` / `_swap2_out` | `swap2(bid1, bid2, ask1, ask2, n_a1, n_a2)` / `swap2_out(..)` | 9,412 B / 9,121 B |

Every actor also has `expire()` (anyone, from the deadline on) and `cancel(sig)` (payer,
SIGHASH_ALL). The redeem sizes include both. For scale: the 8/8 token program is 6.8 kB, a KobAsk
1.7 kB. **The largest scripts are
`TokenSwap_swap2` and `_swap2_out` (9,856-10,143 B):** they observe four orders and two token groups,
and every observed order state is 20 or 21 fields read by the script. A payer who wants a sweep of
one order per leg pays the 6.3-6.6 kB of the single-order swap. `swap2` is the tightest script (227
live bindings and 234 combined stack items of the 244 allowed, measured on the v2.6 router of
2026-10-03; protocol v3 only removed arithmetic from it): its body keeps no local an expression can
stand for (the input index, the sum of the base units sold into the bids and the take of a resting last
ask are written out where they are used), which makes room for the two token handles. The lock pin (2026-10-06) adds
two state fields and two checks to every KCC-20 token intent (one field and one check to a KRON one); argentc, which
refuses an entry over the limit, still compiles every shape, `swap2` included (its peak was not re-measured). The B pin
(2026-10-07) adds one state field to the intents that buy token B and one check per ask escrow; every shape still
compiles. The ask's own `extensionCommitment` (2026-10-07, the last field of `KobAskState`: an ask takes only a custody
of the commitment it names) makes every observed ask state 33 B longer; every shape still compiles, `swap2` included.

`n` is the number of token base units sold into a bid (any amount; the bid enforces its own minimum
fill). How much an ask gives follows from `amount` (the last ask
of a sweep gives `amount` minus what the sold-out asks gave). Hidden witnesses are the handle
prefix/suffix lengths of the imported templates, ask first, then bid, then the token: 4 for the
`KasToToken` and `TokenToKas` actors, 8 for `TokenSwap` (KobAsk 1 and 1,407, KobBid 1 and 1,230,
KobBidKron 1 and 1,090, then the token handles' own: KCC20 8/8 1 and 6,707, 3/3 1 and 2,977, KRON 0 and 2,387 or 2,686);
the `expire` and the `cancel` of a token intent take the locked token's two. Slot limit: at most 3 escrow inputs and 4 token outputs in one token group (the
token has 8/8 slots); the largest transaction (`swap2`: 8 inputs, 2 bids, 2 asks) is 38.8 kB and 53.0k
compute mass, under the 100k standard limit.

What the router pins and what it leaves to the orders. The router observes each order's shape (one
input, zero or one output of its covenant) and pins the token flows: custody change, deliveries,
merchant delivery, payer change. It does not re-check what the order checks itself: payout,
continuation, kill time, tips, auction price, TWAP limits, the order's own stray guard. A resting
order's continuation is not pinned by the router: the ask's continuation state (`amountLeft - n`) and
the bid's byte-identical continuation are the orders' own rules, and the pin on the escrow change
output (which the ask pins too) ties `n` to the token flow. Pinning the continuation as well
costs 21 (ask) or 23 (bid) more live stack bindings per resting order in the compiled script (limit
244 per entry). On the v2.6 router (2026-10-03, argentc `b312ded` with SilverScript v1.0.0) every shape fit
but the two 2 + 2 swaps: `TokenSwap_swap2` peaked at 248 bindings with the ask pin, 250 with the bid pin and
271 with both (227 shipped), `TokenSwapKron_swap2` at 249 combined stack items with the ask pin. The protocol v3
router (no lot arithmetic) compiles with the ask pin in every shape; the bid pin still fails `TokenSwap_swap2`
(245 combined stack items) and both pins 245 live bindings (2026-10-05; `PIN=1 tools/router-gen/gen-router.sh FILE` writes the pinned
variant, `PIN=ask` / `PIN=bid` one side only; `docs/argent-feedback.md` item 6). It would also add a
state copy and a template check to each resting script for a rule the order already enforces, so it
stays out.

What the router relies on in the orders:

- **Exact custody and strays.** A KobAsk's custody UTXO holds exactly `amountLeft` base units; the
  router re-checks it for every ask it observes. Every observed token group has an exact shape (the
  listed escrows and nothing else), so a stray token UTXO owned by an order's covenant id, or any
  other token input, cannot ride along; the orders' own stray guards reject it too.
- **Quotes and rounding.** Amounts are base units, prices sompi per whole token (`scale` base units,
  `docs/spec/order-types.md`). The router lets a keeper take at most `ceil(take * price / scale)` of a
  `KasToToken` intent's KAS per ask (exact split multiplication, the covenants' `quoteOf` rounding up);
  the ask demands `ceil(take * (price(t) - tip) / scale)`, never more for a decaying ask with a tip.
  A rising bid never pays less than its quote, so the payer's bounds are taken at the quote and hold
  at every `t`. The keeper proves `t` to the order (CLTV) and keeps the difference.
- **Not covered:** IOC asks and bids filled partially (the rest goes back to the maker: another
  token shape), and the conditional and if-done orders (`KobCond*`, `KobIfd*`), which are not
  resting limit orders.

### Positional anti-aliasing rule

Argent generates no anti-aliasing. The rule is hand-written and reviewed, and it is the same idea
as the one in the orders: **the intent spent at input j owns exactly two outputs, j and j + 1, and
everything it owes goes there.**

| Intent | out j | out j+1 |
|---|---|---|
| `KasToToken` | payer KAS change (>= locked value - price - `max_extra`) | merchant token delivery |
| `TokenToKas` | merchant KAS (>= `merchant_kas`) | payer token change |
| `TokenSwap` | merchant token B delivery | payer token A change |

Output j is checked by index; output j+1 is the last output of the token group the intent observes
(`OpCovOutputIdx(token, count - 1) == j + 1`; `count` is fixed per entry). Output j+1 is also the
positional slot of whatever input sits at j + 1 (an order pays, delivers or refunds at its own input
index), so **the intent must be the last input** (`tx.inputs.length == j + 1`, every entry).
Without that requirement two attacks work: a KobBid of the merchant or payer placed at j + 1,
which the router does not observe, has its delivery satisfied by the router's own token output
(the keeper takes the bid's KAS), and an observed ask refunded at j + 1 returns its custody into
the merchant output. Every observed
ask must also run its fill: its sigscript must start with the 8-byte push of n > 0 (a refund pushes
n = 0, a cancel a 65-byte signature). Because every observed group has an exact shape and the intent
is last, two intents of the same token cannot share a transaction, and an output cannot be claimed
by two rules: a self-trading payer cannot make one output serve as both its refund
and an order payout, in a single fill or in a sweep, and each intent consumes an output pair of its
own. The harness shows the rule is necessary by ablation: with the change index left to the keeper
and the delivery position not tied to the intent (generated source edited, recompiled), the
self-trade and the double-intent transactions validate (`A-K7`, `A-K8`), and so does the sweep's
self-trade (`A-M3`); on the shipped router the same transactions reject. The sweep bounds are shown
necessary the same way: with the price bound per ask instead of over the sum (`A-M1`), `max_sell`
per bid (`A-M2`) or `max_sell_a` per bid (`A-M4`), a sweep that breaks the payer's limit validates.

### Evidence

`crates/kob-tests/tests/argent_router_tests.rs` runs everything in rusty-kaspa v2.1.0's script engine
with covenant context and script-unit metering: positive transactions, negative cases and
ablation pairs. Positive: R1 (KAS -> token, with an ask tip), R2 (token -> KAS), R3 (token -> KAS ->
token), R4a-c (cancels, a token intent's with its lock), C1 and N-C1-2 (a token intent's cancel without its lock is refused; `c4_router_token_cancel_checks_the_lock_owner`: one that spends another token UTXO of the same token at j + 1 is refused too, and accepted by the actor with the owner check removed), E1-E4 (expiry of each intent kind at and after the
deadline, the expirer keeping exactly `EXPIRE_MAX_FEE`, the payer's cancel before and after the
deadline), R6 (`buy_out`: sold-out, IOC and FOK asks), R7 (`buy2`,
`buy2_out`, `buy3`, `buy3_out`, different quotes, Dutch asks), R8 (`sell_out`, IOC bid, `sell2`,
`sell2_out`, `sell3`, `sell3_out`, rising bid), R9 (`swap_bid_out`, `swap_ask_out`, `swap_out`,
`swap2`, `swap2_out`). Negative (`N-K*`, `N-T*`, `N-S*`): redirected payouts and deliveries,
impostor orders with the same layout, wrong order kind, aliasing, price and amount bounds (single and
summed over a sweep), the presence trap (an owner_scheme 0x04 escrow released by a thief invoking
`cancel`), borrow-enabled escrow, position violations, the wrong shape for the order's state (a
resting entry on a sold-out ask, an ending entry on a bid that continues), custody that is not
exactly `amountLeft`, and strays; `N-E*`: an expiry 1 ms before the deadline, without a time lock or
under a DAA lock, its KAS or tokens to a thief, a fee above `EXPIRE_MAX_FEE` (also out of the lock carrier), tokens returned in part
or with borrowing enabled, and the locked tokens not at input j + 1; `X1`-`X5` (`router_lock_pin`): a stand-in owned by
the intent's id spent as its lock by an expiry (1-unit dust, X1-X2), by a fill (a keeper's own units, X3-X4), or with
the lock's units but another extension commitment (X5), with the ablations `A-X1` / `A-X5` (the amount check, the
extension check removed: the stand-in validates). Every rejection is at the input that should reject.

The other token programs run through the production builders: `crates/kob-protocol/tests/intent_builders.rs` creates,
executes and validates every actor on every program pair of its family (8/8, 3/3, a 3/3 token A with an 8/8 token B,
KRON 2,433 B with an 8/8 token B, KRON 2,732 B with a 3/3 token B; the 3/3 program refuses the three-bid sells at
creation), cancels and expires every one of them (redirected payouts, changes and expiries, an expiry 1 ms early, a
cancel by another key and one without its lock are rejected at the intent input), and measures the cancel and expire
budgets over all of them. `crates/kob-tests/tests/c4_security_tests.rs` (`c4_router_token_cancel_checks_the_lock_owner`)
forges a payer-signed cancel that spends the payer's own tokens in place of the lock, on the 8/8 and 3/3 programs and on
both KRON programs: the router refuses it, and the same actor with the two owner lines of its cancel removed accepts it
(ablation). `production_router_matches_the_harness` ties the harness to the KCC-20 actors the builders pin.

Measured (min fee at 100 sompi/gram, 2026-10-06): R1 (`KasToToken_buy`) is 12.1 kB, 0.024 KAS; R2
(`TokenToKas_sell`) 13.3 kB, 0.027 KAS; R3 (`TokenSwap_swap`) 24.9 kB, 0.050 KAS; the 3-ask sweep
`buy3` 33.2 kB, 0.066 KAS; the largest transaction, the 2 + 2 swap sweep, is 38.8 kB, 53.0k compute
mass, 0.078 KAS. A `cancel` of a `KasToToken` intent reveals only the intent's own actor: 3.0 kB and
0.006 KAS; a token intent's cancel and expiry spend its lock and so carry the token program: 11.4 kB
and 0.023 KAS (`TokenToKas_sell`) to 13.9 kB and 0.028 KAS (`TokenSwap_swap`) on the 8/8 program,
less on the 3/3 and KRON programs (`intent_builders.rs` prints every one). Most of every composed
transaction is the token programs (6.8 kB per 8/8 token input, 3.0 kB per 3/3, 2.4-2.7 kB per KRON).

### The router generator

`contracts/argent/kob_router.ag` is **generated** by `tools/router-gen/gen-router.sh` (pure bash,
LF output) from `tools/router-gen/router_head.ag` (states, helper functions, the reviewed rules in
prose: copied verbatim) and the shape table in the script. Argent has no macros, and the 30 actors
are one pattern at different widths (one order leg per observed order, the bound summed over the
legs, the positional rule, the token pins), so the repetition is written out by the script instead of
by hand. Edit the generator or `router_head.ag`, never the `.ag`:

```
tools/router-gen/gen-router.sh            # write contracts/argent/kob_router.ag
tools/router-gen/gen-router.sh --check    # fail if the committed file is not the generator's output
```

`scripts/build-argent.sh` (and so `scripts/build-contracts.sh`) runs it first: in write mode it
regenerates the file, in `--check` mode a hand-edited or stale `kob_router.ag` fails the build. The
argentc output, the artifact and SHA256SUMS are derived from it.

## Reproducing

```
scripts/build-contracts.sh --check     # silverc artifacts, then contracts/argent (argentc from the pin)
scripts/build-argent.sh --check        # contracts/argent only
argent/build-argentc.sh                # just argentc
cargo test --workspace --locked        # includes the router harness and the artifact checks
```

`scripts/build-argent.sh` stages in `target/argent-stage`, so a failed check leaves nothing in
`contracts/`. The artifacts are meant to be byte-identical on Windows and Linux (CI runs the check on both):
sil2argent drops the absolute paths argentc records.

## Not done / open

- **Patch 0002 is ours.** It implements Argent's own open item "Support artifact-backed app
  dependencies" and is prepared as an upstream PR, rebased onto the module loading rework of
  `b312ded`. Argent is pre-1.0; the pin moves deliberately (`argent/UPSTREAM.md`).
- **No audit.** The handles are final, but nothing is audited: do not pin the artifact in
  production before the audit.
- **Linked handles of an artifact import are not verified by argentc** (see Trust above); the
  pinned artifact id is the defence.
- **Transaction building.** The harness builds composed transactions by hand
  (`encode_entry_sig_script`); validating argent-runtime's `TxBuilder` on the v2 artifacts is open.
  argent-runtime is not used in the browser (as far as we know it has no wasm build); browser and
  keeper flows use `kob-protocol`. Intents are built by hand-written builders there (`router.rs`:
  the router's `sil_abi` embedded and every actor pinned; `build/intent.rs`: create, execute,
  cancel, expire), engine-validated for every shape by
  `crates/kob-protocol/tests/intent_builders.rs` and tied to the harness by
  `production_router_matches_the_harness`; the x402 facilitator executes them (intent mode,
  `docs/spec/x402-swap-and-pay.md` section 17).
- **Interface truthfulness.** `emits` clauses in `KOBOrders.ag` are review-checked against the
  contracts; the exit orders of `KobIfd*` are fresh genesis covenants and are not modelled.
- **Overhead.** The intent adds a 2.6-7.3 kB covenant input to the composed transaction (9.0-10.0 kB
  for the 2 + 2 swap sweep; about 840 B of every token intent is its `expire`); a keeper fee funds
  it. Most of it is the 19- and 21-field order states each `observes` reads; a hand-written
  intent would be smaller still.
- **Observed ranges.** Sweeps are fixed shapes (1-3 orders per leg, 2 for swaps) because argentc
  does not implement `Actor[a..=b]` in an `observes` group yet (announced for the first release),
  so the payer picks the shape at creation; a keeper needing more splits the sweep over several
  intents. IOC orders that are filled partially (the rest returns to the maker) have no entry.
