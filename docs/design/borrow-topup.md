# Consolidating bid deliveries: borrow top-up and accumulating bid (design)

Status: design only, 2026-11-16. Nothing here is shipped or pinned. Base: `origin/master` `d12b28e1b`, worktree
`KOB-wt/borrow-design` (branch `borrow-design`).

## 0. Where this stands (read first)

**Chosen direction (founder, 2026-11-16): section 16.** Accumulate (②) is the default, borrow top-up with a maker-paid tip (③)
is an opt-in, today's new delivery (①) stays for compatibility.

How the document got there:

- Sections 1 to 12: the borrow top-up, the KCC-20 rules and option A. Its incentive is a **maker-paid tip** (`topupTip`),
  which is **accepted** for ③: whoever wants consolidation prepays for it.
- **Rejected:** a charge that the filler pays on a new delivery (considered, not built), and any reliance on an executor's
  default policy. The incentive must come from the contract alone.
- Sections 13 to 15: the accumulating bid and the comparison of all candidates.
- Section 16: the three modes in one template, the mixed batch, and claim-then-sell in one transaction.
- **Section 17: the implementation plan** (final contract specification, work packages, gates, soak, effort). The open
  decisions listed in sections 12, 15 and 16.4 are settled there.

Every claim carries one of three labels:

- **[engine]** verified in the rusty-kaspa script engine by `crates/kob-tests/tests/design/borrow_topup_experiments.rs`
  (section 9 lists the runs);
- **[source]** read in the source at the cited line, not executed;
- **[assumption]** not checked.

Terms. A **receive UTXO** is a KCC-20 holder UTXO owned by a key (owner scheme `0x00`) with borrow scheme `0x01`
(amount-threshold/v1) and guard `threshold (8 bytes LE) ‖ 24 zero bytes`. A **top-up** is a fill that spends the buyer's
receive UTXO on the KCC-20 borrow path and re-creates it with `amount + n`. A **filler** is whoever sends the fill (an
executor, a taker, a keeper).

## 1. Verdict

**Feasible, with three corrections to the idea as it was discussed.**

1. **The mechanism works.** A third party can add tokens to a receive UTXO with no signature of its holder, in the same
   transaction as a KobBid fill and a KobAsk settle. Amount and KAS can only go up; owner and borrow settings cannot change.
   [engine A1, A2, A6, A7, B cross]
2. **Correction: one top-up per token per transaction.** Only the token's leader (its first token input) can take the borrow
   path, so a batch with several buyers of one token tops up one of them and delivers new UTXOs to the others. [engine A4, A5, B3]
3. **Correction: fillers do not prefer a top-up by themselves.** A top-up adds one token input, which reveals the whole token
   program again. The relay fee prices bytes and compute, not storage mass (`crates/kob-protocol/src/tx.rs:752-758` [source]).
   A top-up fill costs the filler **0.0060 KAS more** on `KCC20Ref`, **0.0082** on `KCC20PublicMint`, **0.0146** on
   `KForgeToken7214` and **0.0230** on `KaspaRocketToken11458` than the same fill with a new delivery; storage mass falls by
   about 9% (113,925 to 103,555 in the measured shape). [engine B, C] So the order must pay the filler for it: a small
   maker-set tip (`topupTip`), taken from the delivery carrier the top-up no longer needs.
4. **Correction: the bid must authenticate the receive UTXO by P2SH, not only by template hash.** Without that check a
   look-alike input under the token's covenant id lets a seller keep the buyer's existing balance. [engine D, with ablation]

For the maker the trade is good: a new delivery locks 1 KAS per fill (`DEFAULT_ORDER_CARRIER`,
`crates/kob-protocol/src/defaults.rs:68` [source]) and later costs a merge of about 0.009 to 0.013 KAS per fragment plus a
signature (a 2-to-1 merge is 0.0128 / 0.0172 KAS, [engine A9b]); a top-up costs a tip of about 0.008 to 0.010 KAS, locks
nothing and needs no signature.

The cost is on the project side: a new template generation (every order kind that pins `KobBid` / `KobCondBid` by hash moves
with it), a new input shared between otherwise independent fills, and a new delivery shape through the builder, the planner,
the indexer and the pre-sign decoder. Recommendation in section 11.

## 2. The KCC-20 rules the design rests on

All line numbers are `contracts/kcc20/KCC20Ref.sil`. The published public-mint build has the same `transfer` body; its only
differences are template-checked input reads and the context field in the output state
(`diff` of the two `transfer` entries [source]). `KForgeToken7214` and `KaspaRocketToken11458` are that actor with other slot
bounds (`docs/spec/token-programs.md:235-236`, `:287-288` [source]).

| Rule | Where | Label |
|---|---|---|
| The leader is the first input of the token's covenant id: `OpCovInputIdx(cov, 0) == this.activeInputIndex` | `:111` | [source] |
| At most 2 delegates and 3 outputs (3 / 3); every token output is the leader's | `:114`, `:125`, `:127` | [source] |
| Conservation: `output_total == input_total` | `:144` | [engine B3] |
| The path is chosen by `witness[0]` of the **leader**: `0x00` owner, `0x01` borrow | `:145-149` | [engine A1] |
| amount-threshold borrow: no witness bytes, `threshold = int(guard[0..8])`, negative acts as 0 | `:153-159` | [engine A1], negative [source] |
| Successor is `next_states[0]`, the leader's **first** authorised output; owner, owner scheme, borrow scheme and guard unchanged | `:183-186` | [engine A3, A7] |
| `successor.amount - amount > threshold` (strict) | `:187-188` | [engine A2] |
| `successor KAS >= leader KAS` | `:189` | [engine A6] |
| A delegate has no path byte and always needs its owner: `require_owner_authorized(owner, owner_scheme, witness)` | `:205-217` | [engine A4, A5] |
| A covenant-owned delegate (an ask custody, scheme `0x04`) is authorised by the presence of its covenant's input | `:82-84` | [engine B cross] |
| The owner path works on a borrow-enabled UTXO and may write any successor states (the program does not stop mixing) | `:147-148` | [engine A9, A9b, A9c] |

Consequences:

- **Who may top up:** anyone who brings more than `threshold` units of the same token and extension commitment.
- **How many borrowed inputs:** exactly one per covenant id (token) per transaction, and it must be that token's first input.
  A second receive UTXO in the same transaction is a delegate and needs its holder's signature.
- **Where the tokens come from:** from delegates. A seller's key-owned UTXO (signed) or an ask custody (covenant-owned) both
  work as delegates under a borrowing leader.
- **Slots:** on a 3 / 3 program a top-up leaves 2 token inputs for sellers (today 3) and 3 token outputs as before.
- **A fill of `n <= threshold` cannot top up.** It must use a new delivery. [engine N9]
- **Interaction with KobAsk:** none needed. `KobAsk.settle` identifies its custody by index, owner and exact amount
  (`contracts/orders/KobAsk.sil:176-196`) and lets the token rest sit at any index (`:287-291`); it never requires the custody
  to lead. The cross of a bid and an ask with the receive UTXO leading validates with the shipped `KobAsk`. [engine B cross]
- **Not covered:** `KCC20KaspaCom_0_2_5` (another program, pending review) was not looked at. KRON has no borrow path.

## 3. Contract change

### 3.1 KobBid (prototype: `contracts/prototypes/borrow/KobBidTopup.sil`, +314 B over `KobBid`'s 1,629 B [engine C])

Today the fill pins a new plain UTXO of exactly `n` at output `i` (`contracts/orders/KobBid.sil:235-240`), moves
`deliveryCarrier` out of the escrow (`:258`) and puts it on the delivery (`:259`).

**New state fields, appended after `decayStep`** (so every existing field offset stays):

| Field | Meaning |
|---|---|
| `recvThreshold` (int) | 0: plain deliveries only, exactly today's behaviour. > 0: the maker accepts deliveries into receive UTXOs whose threshold is at least this |
| `topupTip` (int) | sompi a top-up fill may leave to the filler; `0 <= topupTip <= deliveryCarrier` |

**New fill argument** `form`, last: `fill(byte[8] nb, int tokenTplIn, int t, int form)`. `nb` stays the first push, so trigger
evidence reads are unchanged (`KobBid.sil:22-24`).

| `form` | Delivery at output `i` | Escrow | Allowed when |
|---|---|---|---|
| 0 | new plain UTXO of `n` (byte-identical to today) | releases `deliveryCarrier` onto the delivery | always |
| 1 | new receive UTXO of `n`, threshold `recvThreshold` | releases `deliveryCarrier` onto the delivery | `recvThreshold > 0` |
| 2 | top-up: the state of input `tokenTplIn` with `amount + n` | releases `topupTip`, to the filler; nothing to the delivery | `recvThreshold > 0` |

Form 0 stays available on an opted-in order because router executions pin a plain delivery of exactly `n1` and their own token
input and output set (`contracts/argent/kob_router.ag:805-856` [source]); an opted-in bid must stay fillable by them.
[engine N6c]

**Form 2 checks** (prototype lines in the block starting "Top-up."):

1. `tokenTplIn` is an input of the token's covenant id and its template bytes hash to `tokenTplHash` (existing checks,
   `KobBid.sil:230-234`).
2. **New:** the input's script is the P2SH of the redeem script at the end of its signature script (the check
   `KobAsk.tplRs` makes for a custody, `KobAsk.sil:123-129`). The state is read from the signature script, so this is what makes
   it the UTXO's real state. [engine D]
3. State bytes `[9..47)`: owner is `maker`, owner scheme `0x00`, borrow scheme `0x01`. [engine N2, N7]
4. `int(guard[0..8]) >= recvThreshold` and `guard[8..32) == 0`. [engine N3, N3b, N3c]
5. Extension commitment equals the order's.
6. Output `i` is `P2SH(prefix ‖ 0x08 ‖ (have + n) ‖ state[9..112) ‖ suffix)` bound to the token's covenant id. [engine N1]
7. Output `i` holds at least `KAS of the receive UTXO + used - allIn` (continuing), or that plus the unspent remainder when the
   bid ends without a refund output. [engine N4, terminal]
8. The continuation holds exactly `left - topupTip`. [engine N5]
9. When the bid ends: `rem = inValue - allIn - topupTip`; a `rem >= REFUND_MIN` goes to the maker through `refundTo` as today,
   a smaller one rides on the delivery, and a negative one is refused (the filler uses form 0 or 1), so a top-up never takes
   KAS from the receive UTXO.

`canContinue` (`KobBid.sil:248`) is unchanged: it keeps reserving a full `deliveryCarrier` per remaining fill, so the escrow
of an opted-in order always covers a form 0 or form 1 fill. Carriers a top-up did not use come back at the end.

**What the bid does not check, on purpose:** that `tokenTplIn` is the leader, that it was spent on the borrow path, or that
`n > threshold`. The token program enforces those when nobody signed for the receive UTXO, and if its holder signed, the
holder chose the outputs.

**How the receive UTXO is identified:** by the filler, with the input itself as the proof. A state field cannot pin it,
because its outpoint changes with every top-up.

**Who creates the first one:** the first form-1 fill. No wallet step is needed.

### 3.2 Maker-safety argument

- A receive UTXO that nobody signed for can only be spent as the token's leader on the borrow path, and then the token program
  keeps its owner, settings and KAS and raises its amount by more than the threshold (section 2). So a maker's existing balance
  survives any transaction the maker did not sign.
- The bid at input `i` owns output `i` (positional rule), so two bids never share a delivery. Its delivery holds `have + n`,
  where `have` is the authenticated amount of a UTXO this transaction consumes.
- Two bids of one maker naming the same receive UTXO each demand `have + n`. The transaction then needs `have` extra units from
  the seller: it validates only as a gift to the maker. [engine B3]
- The one case where the two outputs could be the same output and the read amount could be too small is a look-alike input.
  Check 2 closes it. [engine D: with the check removed the transaction validates and the seller keeps 1,000 units]

### 3.3 Other templates

| Template | Delivers tokens to a key? | Proposal |
|---|---|---|
| `KobBid` | yes, `KobBid.sil:235` | phase 1, as above |
| `KobCondBid` | yes, same hand pin, `KobCondBid.sil:457-462` | phase 1, same fields and `form` argument on its settle. Its state is spliced by `KobIfdAsk` as the exit state (321 B, `kob_v2_buy_tests.rs:1582`), so the two fields go at the end and `KobIfdAsk` gains the same two fields to hand to its exits |
| `KobIfdAsk` | no (sells first; its exit is a `KobCondBid`) | phase 1: state carries the exit's two fields only |
| `KobIfdBid` | no: the bought tokens go into the exit's custody, covenant-owned and borrow-disabled (`KobIfdBid.sil:432-446`) | unchanged behaviour (invariant P2) |
| `KobAsk`, `KobCondAsk` | only refunds, cancels and IOC returns to the maker | unchanged: one event per order, and a refund must not depend on a third input |
| `KobPair`, `KobCondPair`, `KobIfdPair` | yes, token T at output `i` (`KobPair.sil:367-370`) | phase 2. Same pattern per delivered token; needs its own slot and layout analysis because the rest goes back at the custody's own index |
| Router intents | pin plain deliveries and locks (`kob_router.ag:284-288`, `:837-856`) | unchanged. x402 refuses borrow-enabled UTXOs by design (`docs/spec/x402-kcc20-profile.md:265-276`) |
| KRON twins | KRON has no borrow path | out of scope. Their sources do not change |

### 3.4 Version bump and retirement

- Every byte of a contract is in its template hash (`docs/spec/template-retirement.md:3-4`). `KobCondBid`, `KobCondAsk`,
  `KobIfdBid`, `KobIfdAsk`, `KobCondPair` and `KobIfdPair` inline the `KobAsk` / `KobBid` template hashes as trigger evidence
  (`kob_v2_buy_tests.rs:370-377` [source]), and the router is built against the `KOBOrders::KobBid` handle. So the KCC-20 side of
  the generation moves together, as in the two earlier retirements (`template-retirement.md:41-44`). `KobAsk`, `KobPair` and the
  KRON twins keep their bytes unless they pin a changed hash; that is checked when the artifacts are rebuilt. [assumption for
  the exact list]
- Steps: rebuild `contracts/artifacts`, update `artifacts::PINNED` (`crates/kob-protocol/src/artifacts.rs:533`), add the old
  bytecode to the cancel-only table (`crates/kob-protocol/data/retired/`, `scripts/extract-retired.mjs`), regenerate
  `contracts/SHA256SUMS` and the deploy records (`scripts/build-deploy.sh`), re-pin the router.
- KOB1 payload: the record names family and kind, not a hash, and the compact state follows the pinned layout
  (`docs/spec/kob1-payload.md:89-100`, `:140-142`). Two more fields change the `KobBid` state length (285 B today, `:152`) and
  the documented table. No payload version bump was found to be required. [source, second-hand]
- Before mainnet launch there is nothing to migrate on mainnet. After launch it needs a published migration window
  (`template-retirement.md:52-56`).

## 4. Receive-UTXO state and wallet policy

- **State:** owner = holder key, owner scheme `0x00`, borrow scheme `0x01`, guard = threshold LE ‖ 24 zero bytes, the token's
  extension commitment. Other borrow schemes (signature, hash chain) are not used: a filler must be anyone.
- **Threshold = the price of one grief.** Each third-party spend must add more than `threshold` units, and they stay with the
  holder. The bid accepts any receive UTXO whose threshold is at least the order's `recvThreshold`, so orders with different
  values can share one UTXO.
- **Proposed wallet rule:** one threshold per token per wallet, set at first use to the amount worth about half the default
  minimum fill (the default minimum fill is 10 KAS of notional, `defaults.rs:81`), and reused for every later order of that
  token. Fills are at least `minFill`, so they clear it; a per-order `minFill - 1` would give every order its own threshold and
  split the holdings again. The exact value is a founder decision (section 12).
- **Opt in / out:** per order, `recvThreshold = 0` is today's behaviour. A wallet setting chooses the default for new orders.
  A holder leaves by spending the receive UTXO on the owner path into a plain UTXO ("Convert to plain", one signature).
- **KRON:** not available.

## 5. Executor, matcher, indexer

What exists today (all [source], from the survey of `crates/kobd` and `crates/kob-protocol`):

- The leader is the first token input and `next_states` are the token outputs in output order (`kob-protocol/src/tx.rs:1117`,
  `:1307-1308`, `:1348-1356`). Today's leader is always a seller-side input.
- The witness is only `CovenantId` or `P2pk` (`tx.rs:204-209`); `script.rs:54-55` hard-codes the path byte `0x00`; budget roles
  are `{program}.leader.{covid|p2pk}.i{n}.o{m}` (`tx.rs:1471-1472`).
- Slot cost per candidate: a bid is `tin = 0, tout = 1` (`kobd/src/matcher/batch.rs:1079-1090`).
- Terminal bids with a refund must be the first inputs (`tx.rs:1520-1524`, `docs/spec/matcher.md:334-340`).
- The indexer keeps every proven token output per owner (`indexer/schema.sql:244-266`, live-owner index `:308`), with the borrow
  fields inside the state blob only (`indexer/packed.rs:161-162`). It follows the confirmed chain; no mempool view.
- The matcher chains its own unconfirmed outputs (`matcher/chain.rs:3-12`) and backs off per order on a double spend, but only
  order inputs are mapped to an order (`matcher/run.rs:750-757`). No path rebuilds a fill in another shape (`matcher/engine.rs`).
- Fill accounting derives the price from `escrow_before - escrow_after - delivery_value - refund_value`
  (`indexer/processor.rs:1833-1869`).

Changes:

1. **Builder (`kob-protocol`)**: `Witness::Borrow` (leader only, path byte `0x01`), budget roles `leader.borrow.*` and
   regenerated tables; bid legs carry `form`; layout rule per token: at most one top-up leg, its receive UTXO is laid out before
   every other input of that token, and the top-up leg is that token's lowest-index bid leg (its delivery must be the first token
   output). If the token has a refund-carrying terminal bid in the batch, only that bid may be the top-up leg.
2. **Planner**: a top-up bid costs `tin = 1`. Choose form 2 when a usable receive UTXO exists, the slots allow it, and
   `topupTip` covers the extra relay fee (program redeem length × the fee rate); otherwise form 1 (or 0 when
   `recvThreshold == 0`).
3. **Choosing the UTXO**: among the maker's live receive UTXOs of the token with the order's commitment, `threshold >=
   recvThreshold`, `threshold < n` and a canonical guard, take the largest amount (ties: lowest outpoint). Every executor then
   aims at the same UTXO, so the holdings converge to one. The lookup is one indexed seek by owner plus a decode filter, the
   pattern `indexer/book.rs:101-138` already uses; the view hook exists (`matcher/book.rs:217-221`).
4. **Chaining**: the successor outpoint is `(txid, bid leg index)`; track it like a continuation so the next fill of the same
   maker can top it up before it is confirmed.
5. **Conflicts and fallback**: map the receive UTXO input to its top-up leg. When the node names it in a double-spend or
   missing-input rejection, do not back off the order: mark `(maker, token)` busy for a short time and rebuild the fill with
   form 1 at once. This alternate-shape retry is new code.
6. **Reorgs**: a top-up chain is ordinary chained spending; `revert_block` already restores holdings
   (`indexer/processor.rs:2609`, `:2678-2679`). New coupling: fills of different orders of one maker now depend on each other
   through the receive UTXO, so a dropped parent invalidates them together. The busy mark and form 1 bound the damage.
7. **Indexer**: fill accounting must take the delivery's KAS net of the receive UTXO's KAS and treat `topupTip` as paid out;
   trade size comes from the fill argument `n`, not from the delivery amount. Holder UTXO views must always carry the state
   (the web tracker synthesises a plain state when it is missing, `web/src/data/token-tracker.ts:356-361`).
8. **Takers and keepers** (CLI `kob take`, maintenance sales): the same builder option. They pay the extra fee and receive the tip.

## 6. Option A: holders spend every key-owned holding on the owner path

Mandatory once fills can create receive UTXOs, and wanted already today: a testnet-10 holder's token (WBLF) carries borrow
scheme `0x02` and cannot be sold in the app. **Option A covers every key-owned holding (owner scheme `0x00`), whatever its
borrow scheme and guard**, not only amount-threshold with a canonical guard. The program allows all of it [engine A9, A9b, A9c,
J]; today the builders refuse it.

Verified on `KCC20Ref` and `KCC20PublicMint` for borrow schemes `0x01` (amount threshold), `0x02` (Schnorr signature guard)
and `0x03` (hash chain) [engine J]: the owner key alone places a `KobAsk` from such a holding (plain covenant-owned custody,
change with the same scheme and guard), a taker then buys that custody, and the owner merges two holdings of the same settings
into one of the same settings. The guard's key cannot spend it on the owner path, as leader or as delegate.

**No borrow scheme needs more than the owner path.** The owner path checks the owner only (`KCC20Ref.sil:147-148`); a borrow
witness exists only on the borrow path, and a hash-chain guard does not advance on an owner spend. What option A does not
cover is another **owner** scheme: holdings owned by a key hash (`0x01`, `0x02`), a script (`0x03`) or a covenant
(`0x04`) need their own witness, and the builders spend owner scheme `0x00` only. Not checked: `KCC20KaspaCom_0_2_5`.

Difference between the schemes for the wallet: a scheme-`0x01` holding can be re-created by anyone who adds tokens, a
scheme-`0x02` or `0x03` holding only by the holder of the guard key or chain secret, and always only upward. So the "no
pre-signed flows" rule below applies to all three, with the guard holder as the only possible griefer for `0x02` / `0x03`.

| Where | Today | Change |
|---|---|---|
| `crates/kob-protocol/src/build.rs:849-851` `check_token_input` | refuses any holder input that is not plain: the hard stop for every builder | accept every borrow scheme and guard on key-owned (owner scheme `0x00`) holder inputs; keep refusing on custody and strays |
| `crates/kob-protocol/src/state.rs:625`, `:889-892` (`Kcc20State::p2pk`, `TokenState::user`) | every holder output is plain | a constructor that copies the inputs' borrow scheme and guard for the **change**; recipients, custody and deliveries stay plain |
| `build.rs:921/960`, `:996/1037`, `:1186/1240`, `:1745-1776`, `:3520/3668`, `build/intent.rs:179/234` | change built plain; mixed extensions refused | change keeps the settings; inputs of different (scheme, guard) refused, next to the existing mixed-extension refusals |
| `web/src/kob/token-state.ts:69` `isPlainState`, `web/src/kob/orders/common.ts:230` `usableTokenUtxos`, `web/src/kob/orders/pair-common.ts:121`, `web/src/kob/funding.ts:127`, `web/src/kob/cancel.ts:756` | plain only | group by (scheme, guard); select from one group; plain group first |
| `web/src/kob/merge.ts:162`, `:350-352` | plain inputs, plain merged output | one merge tree per group; merged output keeps the group's settings |
| `web/src/kob/decode.ts:1640-1645`, `:970-971` (`token-state-unplain`, blocking) | any non-plain token output blocks signing, in the app and in the private-key signer frame (`web/src/signer/summary.ts:408`) | allow only on the maker's own change, and only when its (scheme, guard) equals that of every maker token input of that token; still blocking for recipients and custody |
| `decode.ts:657-674` | no check on inputs | add the no-mixing check |
| `crates/kob-cli/src/trade/plan.rs:1184`, `merge.rs:92`, `:166` | plain only | as the web app |
| `web/src/kob/balances.ts:190`, `ui/market/held-list.ts:55` | already count borrow-enabled holdings as free | none |
| `ui/ticket/OrderTicket.tsx:273-275`, `i18n/en/ticket.ts:19` | excluded from the ticket balance with an "unusable: borrow" hint | counted; the hint remains only for other owner schemes |
| `crates/kob-x402` (`src/token.rs:361`, `:380-386`, `:526`; `src/swap.rs:443`; `src/client/token.rs:215`) | refuses borrow-enabled inputs and outputs | unchanged. The wallet offers "Convert to plain" before an x402 payment |

Rules:

- Never mix inputs whose (scheme, guard) differ. To combine groups, convert one explicitly.
- A custody is always plain (P2). An ask placed from a receive UTXO has a plain custody and a receive-state change.
- Flows that sign now and broadcast later (x402, cancel-replace prepared in advance) must not use receive UTXOs.
- Display: one balance; a small mark on amounts held in receive UTXOs; "received by top-up" in the history.

## 7. Security

| Topic | Finding |
|---|---|
| Steal from the holder | Not possible through the token program: amount only grows, KAS never falls, owner and settings fixed. [engine A2, A6, A7] |
| Under-delivery by a fill | Refused by the bid. [engine N1, N4, N5, terminal] |
| Delivery to someone else or into a cheaper-to-churn UTXO | Refused by the bid. [engine N2, N3, N3c] |
| Look-alike receive state | Refused by the P2SH check; without it the theft validates on `KCC20Ref`. [engine D + ablation] Needs a non-template UTXO under the token's covenant id, which only a contaminated genesis has; tokens are tradable with `genesis_unverified` (`docs/spec/token-programs.md:292-293`), so the check is required, not optional |
| Lock funds | No. The owner path always works. [engine A9] |
| Outpoint churn (griefing) | Anyone can re-create a receive UTXO by adding more than `threshold` units (a pure churn with gain 0 is refused [engine A2]). It breaks any transaction of the holder that names the old outpoint. Cost per churn: `threshold + 1` units given to the victim plus about 0.013 to 0.017 KAS of fee [engine A1]. Honest fills churn it too |
| Holder's own spend races a fill | The holder's signed transaction or the fill loses and is rebuilt. For the holder that is one more signature. Mitigations: the wallet spends plain UTXOs first; automatic rebuild on "input spent" |
| Front-running a filler | Another top-up or the holder's spend makes the filler's transaction invalid. The filler pays nothing for a rejected transaction and falls back to form 1 |
| Executor funds | Unchanged: the executor adds no funds to a receive UTXO beyond the tokens the bid pays for |
| P2 custody exactness | Unchanged. No custody becomes borrow-enabled; `KobAsk` is untouched and still refuses a borrow-enabled custody (`KobAsk.sil:187`). Red-team BOR-01 (`docs/security/REDTEAM-2026-11-kaspero.md:58`) is rerun on the new generation |
| P1 maker value | The price paid is unchanged. New bounded charge: `topupTip <= deliveryCarrier` per top-up fill, bounded in number by `minFill` like the carriers (P10) |
| P4 anti-aliasing | Delivery still at the bid's own index. [engine B3] |
| P8 trigger evidence | `nb` stays the first push; the evidence orders are re-pinned to the new hashes |
| x402 / Argent | Router executions use form 0 and pin plain deliveries; x402 refuses receive UTXOs. Both unchanged |
| Third-party wallets and explorers | Unknown how KasWare, Kaspire or kascov show or spend a borrow-enabled holder. [assumption] Check on testnet-10 before deciding |

Negative tests to add (each with its ablation: remove the check, the attack must pass):

- every N-case and D of section 9 as catalog entries on `KCC20Ref`, `KCC20PublicMint`, `KForgeToken7214`, `KaspaRocketToken11458`;
- form 2 with: wrong extension commitment; owner scheme `0x01`; borrow scheme `0x02` / `0x03`; negative threshold; `have + n`
  overflow; `topupTip > deliveryCarrier`; negative `recvThreshold` or `topupTip`; a terminal fill whose `rem` is negative;
- form 2 where the receive UTXO is not the leader (signed delegate: accepted; unsigned: refused by the token);
- form 2 delivery placed after another token output of the token (refused by the token);
- a refund output (`refundTo`) together with a top-up; a top-up next to an IOC return and next to a stop arm;
- the same for `KobCondBid`, and `KobIfdAsk` exits inheriting the two fields;
- c4 / c6 fuzz seeds with `form` in {0, 1, 2} and receive UTXOs in the wallet set;
- pre-sign decoder: receive-state change accepted only with equal settings on every maker input; refused on a recipient.

## 8. Test plan and gates

1. Engine suites: the prototype cases promoted into `kob_v2_tests` / `kob_v2_buy_tests` style catalogs, on all four published
   KCC-20 programs; `tools/ablation` confirms every new check.
2. Builder: golden vectors for the new shapes; `KOB_REGEN=1` budget and tip tables; `carrier_fees.rs` and `cost_table.rs`.
3. kobd: planner property tests (slots, one top-up per token, refund ordering), conflict and fallback tests, a flow test
   (ask, bid, top-up fills chained, holder sells from the receive UTXO), reorg test.
4. Web and CLI: option A planners, merge per group, decoder and signer-frame cases, vitest.
5. Re-pin: artifacts, `PINNED`, retired table and `retired_cancel_tests`, `contracts/SHA256SUMS`, deploy records, router.
6. Audit pass over the changed templates (P1 to P14) and a red-team rerun of the borrow and family packs.
7. Testnet-10 soak with top-up on by default, third-party wallet check, then the mainnet decision.

## 9. Experiments run

File: `crates/kob-tests/tests/design/borrow_topup_experiments.rs`, prototype `contracts/prototypes/borrow/KobBidTopup.sil`,
both uncommitted to master. Command: `cargo test --release -p kob-tests --test borrow_topup_experiments -- --nocapture
--test-threads=1`. Result: 6 tests pass. A and B ran on `KCC20Ref` and `KCC20PublicMint`.

| Id | Scenario | Result |
|---|---|---|
| A1 | third party tops up: borrow leader + seller-signed delegate | accepted |
| A2 | gain = threshold / threshold + 1 / 0 | refused / accepted / refused |
| A3 | successor as the second token output | refused |
| A4 | receive UTXO as an unsigned delegate | refused |
| A5 | two receive UTXOs of two buyers in one transaction | refused, only at the second one |
| A6 | successor with 1 sompi less / more KAS | refused / accepted |
| A7 | successor changes owner, owner scheme, borrow scheme, threshold up or down | all refused |
| A8 | borrow path on a plain holder | refused |
| A9, A9b, A9c | owner path: change keeps settings; merge of mixed settings; receive UTXO as a signed delegate | accepted |
| B | taker sells into the bid: form 0, form 1, form 2 | accepted |
| B | bid × `KobAsk`, form 1 and form 2 (receive UTXO leads, custody delegates) | accepted |
| B | terminal top-up; 1 sompi less on the delivery | accepted; refused |
| N1 to N9 | section 3.1 checks | refused where marked; N3b and N6c accepted |
| B3 | two buyers: one top-up, one new | accepted |
| B3 | two buyers, both topped up | refused, only by the token |
| B3 | one buyer, two bids on one receive UTXO | accepted only as a gift of `have`; refused when funded with `n1 + n2` |
| C | cost on `KForgeToken7214`, `KaspaRocketToken11458`; script size | see below |
| D | look-alike receive state | refused only by the bid; with the P2SH check removed the whole transaction validates |

Relay fee at the floor rate, bid × ask, new delivery against top-up (KAS):

| Program | New | Top-up | Extra |
|---|---|---|---|
| `KCC20Ref` | 0.01528 | 0.02123 | 0.00595 |
| `KCC20PublicMint` | 0.01751 | 0.02569 | 0.00819 |
| `KForgeToken7214` | 0.02387 | 0.03843 | 0.01455 |
| `KaspaRocketToken11458` | 0.03236 | 0.05540 | 0.02304 |

The larger bid script adds about 0.0006 KAS to every fill of the new template, in any form (314 B).

Not tested: `KobCondBid`, pair orders, the builder, the planner, any network run.

## 10. Effort (rough, working days)

| Part | Days |
|---|---|
| Option A: builders, web, CLI, decoder, tests | 4 to 5 |
| Contracts: `KobBid`, `KobCondBid`, `KobIfdAsk` fields; catalogs and ablation | 4 to 5 |
| Re-pin of the generation, retired table, deploy records, router | 2 |
| `kob-protocol` builder: borrow witness, layout rule, state and payload fields, budgets, vectors | 4 |
| kobd: lookup, selection, chaining, conflict fallback, fill accounting, API | 5 to 7 |
| Web: opt-in setting, order fields, display, convert-to-plain | 2 to 3 |
| Audit pass, red-team rerun, testnet soak | 4 to 6 |
| **Total** | **about 5 weeks**; option A alone about 1 week |

Phase 2 (pair deliveries) is not included.

## 11. Recommendation (first version, superseded by section 15)

1. Ship **option A** first. It has no contract change and removes the "unusable: borrow" dead end that exists today for any
   borrow-enabled UTXO someone sends to a user.
2. Do the template change **only bundled with a re-version that is happening anyway**, ideally the last one before mainnet,
   because it moves the whole KCC-20 generation and needs the audit and red-team rerun.
3. Phase 1 scope: `KobBid` and `KobCondBid` (with `KobIfdAsk` carrying the fields). Pairs later. Router, x402 and KRON stay as
   they are.
4. If the re-version is not wanted now, the cheap partial answer is wallet-side: add the user's spare fragments as extra signed
   inputs whenever the user signs a token transaction anyway (sell, ask placement, taker buy). It needs no contract change, but
   it does nothing for a maker who only rests bids.

## 12. Decisions for the founder (first version, superseded by section 15)

1. Go or no-go for the template change, and when: before mainnet, or with a later re-version.
2. Filler incentive: a maker-set `topupTip` field (proposed), and its wallet default (proposed: 1.25 × the measured extra fee
   of the token's program).
3. Threshold policy, that is the price of one grief: proposed one value per token per wallet worth about 5 KAS at first use.
4. Default for new bids in the web app: receive UTXOs on or off.
5. Scope of phase 1 (KobBid + KobCondBid) and whether pairs follow.
6. Whether option A ships on its own first, and whether the wallet-side "merge while trading" is wanted as well.
7. x402 keeps refusing receive UTXOs (proposed), with "Convert to plain" in the wallet.

## 13. The accumulating bid

Prototype: `contracts/prototypes/borrow/KobBidAcc.sil` (2,251 B against `KobBid`'s 1,629 B). Experiments: Part F of
`crates/kob-tests/tests/design/borrow_topup_experiments.rs` (ids F and G below), all [engine] on `KCC20Ref` and
`KCC20PublicMint`; costs also on `KForgeToken7214` and `KaspaRocketToken11458`.

### 13.1 Idea

The bid keeps what it buys in its own token custody, exactly as `KobAsk` keeps what it sells
(`contracts/orders/KobAsk.sil:176-196`: one UTXO owned by the order's covenant id, scheme `0x04`, borrow disabled, known by its
exact amount). There is no borrow path, no UTXO shared with the holder's own spends, and one delivery form.

| Event | Token side | KAS side |
|---|---|---|
| First fill (`bought == 0`) | output `i` = custody of `n`, owned by the order id | the escrow puts `deliveryCarrier` on it, once |
| Later fill | the custody is an input (`custIn`); output `i` = custody of `bought + n` | nothing but the price leaves the escrow; the custody keeps its KAS |
| Fill that ends the bid | output `i` = `bought + n` to the maker's key, one plain UTXO | spare KAS to the maker as today (`refundTo` or on the delivery) |
| Refund (anyone, after expiry / idle / IOC kill) | the whole custody to the maker's key at the custody's own index | escrow to the maker at output `i`, less at most `refundTip` |
| Cancel (maker's signature) | wherever the maker signs it | same |
| Claim (maker's signature, the `cancel` entry) | custody to the maker; the order continues under its id with `bought = 0` | the maker adds one carrier for the next custody |

### 13.2 Contract

- **One new state field, `bought`, last and mutable.** The continuation is this script with `bought + n` spliced into its
  8-byte window, checked by script equality, the way `KobAsk.contSpk` splices `amountLeft` (`KobAsk.sil:161-168`). [engine F:
  the window is asserted; G6 a stale `bought` is refused]
- **The custody is known by its exact state**: P2SH of the token template, amount `== bought`, owner = the order id, scheme
  `0x04`, borrow disabled, zero guard, the order's commitment. [engine G2, G5, G9]
- **Strays.** Today `KobBid.noStrays` refuses every token input owned by the order id (`KobBid.sil:121-133`). The prototype
  takes `KobAsk`'s form (`KobAsk.sil:143-155`): it skips exactly one input, the authenticated custody, and only when
  `bought > 0`. A look-alike or a second UTXO of the order's id can therefore neither stand in for the custody (wrong amount,
  G2) nor ride along (G10). A stray of exactly `bought` units is interchangeable with the custody and is a gift.
- **No choice of form.** A continuing fill that delivers to the maker's key, on the first fill or later, is refused, and so is
  a later fill that starts a second custody. [engine G3, G4, G4b]
- **The order never ends with tokens left behind.** The ending fill must pay `bought + n` to the maker (G8, G8b, G8c); the
  refund must spend the custody as the only token input and pay all of it, with its KAS, to the maker (G11, G12, G13). Today's
  refund carries no token input (`KobBid.sil:309`); with `bought == 0` it still does not.
- **Carrier.** `canContinue` reserves a carrier only until the custody exists. A bid is funded with one delivery carrier, not
  one per possible fill (today up to `ceil(N / 10)` KAS for a notional of `N` KAS, `defaults.rs:72-77`).
- **IOC / FOK** bids end with their fill, so they deliver to the maker directly, as today.
- Not prototyped: an `accumulate` flag (0 = today's behaviour, per order, the maker's choice); a dedicated `claim` entry; the
  ending fill with a refund output next to a custody (the code path is today's `refundTo`).

### 13.3 Cost per fill (relay floor, KAS; taker sells into the bid; bid x ask gives the same differences)

| Program | Today (`KobBid`) | Accumulating, first fill | Accumulating, later fill | Borrow top-up (section 9, plus its larger script) |
|---|---|---|---|---|
| `KCC20Ref` | 0.01098 | 0.01223 (+0.0012) | 0.01818 (+0.0072) | +0.0067 |
| `KCC20PublicMint` | 0.01321 | 0.01446 (+0.0012) | 0.02265 (+0.0094) | +0.0089 |
| `KForgeToken7214` | 0.01958 | 0.02082 (+0.0012) | 0.03538 (+0.0158) | +0.0153 |
| `KaspaRocketToken11458` | 0.02807 | 0.02931 (+0.0012) | 0.05235 (+0.0243) | +0.0237 |

- The extra cost of a later fill is the custody input, which reveals the token program once more: the same cost as a borrow
  top-up, to within 0.0006 KAS. Storage mass falls (bid x ask 114,445 to 104,215).
- **Who pays:** the filler, on every fill after the first, out of the spread and the tip. Against the default minimum fill of
  10 KAS that is 0.07% (`KCC20Ref`) to 0.24% (KaspaRocket). A bid at a thin spread on a large program is filled a little later
  or needs a slightly higher tip. For comparison, merging one fragment later costs the maker about 0.009 to 0.013 KAS on the
  3 / 3 programs plus a signature, and each fragment locks 1 KAS until then.

### 13.4 Batches on a 3 / 3 program

A custody is covenant-owned, so it can be a delegate: there is no "one per token" limit as with borrow. Each bid that already
has a custody uses one token input and one token output.

| Batch against one ask | Today | Accumulating (later fills) |
|---|---|---|
| Bids per transaction, ask sold out | 3 | 2 |
| Bids per transaction, ask rests | 2 | 2 [engine F] |
| A third bid | fits when the ask sells out | 4 token inputs: refused by the token, every order accepts [engine F] |
| Asks per bid | 3 | 2 |

First fills use no token input and count as today. Larger crossings chain, as now.

### 13.5 What else changes

- **Indexer:** a bid becomes a custody-holding order after its first fill, so its custody is learned from fills, not from the
  placement record (`docs/spec/kob1-payload.md:245-250` covers placement only). Order-owned token UTXOs are already tracked
  (`indexer/schema.sql:193-209`), and the matcher already maps a custody to its order for conflicts (`matcher/run.rs:750-760`).
  The order view gains `bought`.
- **Matcher:** a bid costs `tin = 1` once it has a custody (`matcher/batch.rs:1079-1090`); the custody's next outpoint is
  chained like an ask's rest. No lookup of the buyer's holdings, no fallback shape, no new witness or budget role.
- **Web / CLI:** balances show "bought, held by the order" as the buyer's; a Claim action; cancel returns escrow and tokens;
  the pre-sign decoder checks that a cancel or claim sends the custody to the maker. Option A (section 6) is not needed.
- **Invariants:** P2 changes from "bid-side orders own no tokens" to custody exactness for bids, with the same checks as asks.
- **Maker liveness:** bought tokens are not in the wallet until the bid ends, is cancelled or is claimed. A claim is one signed
  transaction (0.0117 KAS on `KCC20Ref` [engine F]) and, like an amend, competes with a fill in flight.
- **Result per buyer:** one UTXO per order, not one per buyer. A ladder of k levels ends as k UTXOs (today: one per fill).
- **KobCondBid:** the same pattern fits (it already splices `amountLeft`); phase 1 or 2, a scope decision.
- **KobIfdBid** already accumulates into a covenant custody (`KobIfdBid.sil:432-446`; repeat merges into the custody,
  `docs/spec/matcher.md` section 9, RPB4), so the shape has precedent in the builders.
- **Pairs:** a pair order would need a second custody for the bought token; later.
- **Router:** a `TokenToKas` sell pins a delivery of exactly `n1` to the bid maker's key and its own lock as the only token
  input (`contracts/argent/kob_router.ag:805-856` [source]). It cannot fill an accumulating bid until the router gets a
  bid-custody leg (it has ask-escrow legs to copy). Until then router sells only reach non-accumulating bids.
- **Version bump:** the same generation-wide re-pin as section 3.4.

## 14. Candidates compared

| | Today | Borrow top-up (1 to 12) | Accumulating bid (13) | Bid pins its previous delivery | Per-buyer vault covenant | Wallet merges while trading |
|---|---|---|---|---|---|---|
| UTXOs per buyer and token | one per fill | one (a few after conflicts) | one per order | one per order | one | fewer, only for users who keep signing |
| Extra relay fee per fill | none | +0.006 to +0.023 on a top-up | +0.007 to +0.024 after the first fill | as borrow | as borrow or more | none (the user's own transactions grow) |
| Carriers locked | one per fill | one | one per order | one per order | one | one per fill until merged |
| Filler has a choice | no | yes: paid for by the maker's tip (accepted for ③); a filler-paid charge was rejected | no | no | yes, or it stalls | no |
| Griefing | none | outpoint churn for a gift above the threshold; breaks pre-signed spends | none new | churn or the maker's own spend breaks the pin | as borrow | none |
| Liveness of fills | always | always (new delivery as fallback) | always; conflicts only on the bid's own UTXOs, as today | **stalls** when the pinned UTXO is spent | stalls or needs a fallback | always |
| Buyer can spend at once | yes | yes, racing with fills | no: at the end, on cancel or by claim | yes, and that breaks the pin | by a vault withdrawal | yes |
| Bids per 3 / 3 transaction | 3 | 3, one of them topped up | 2 | 2 | 1 (shared input) | 3 |
| Contract change | none | 2 fields, 3 forms, P2SH read; option A in every builder and wallet | 1 mutable field, custody checks copied from `KobAsk`, refund returns the custody | borrow plus outpoint reads | a new covenant plus order changes | none |
| Router / x402 | works | works (plain form) | sells skip such bids until the router gains a leg | no | no | works |
| Effort | 0 | about 5 weeks | about 3.5 weeks | more than borrow | most | about 1 week |

Notes on the two rejected structural options:

- **Bid pins its previous delivery** (the continuation and the delivery come from the same transaction, so the next fill could
  demand "the token input created next to me"). The delivery is the maker's own UTXO: when the maker sells it, or anyone churns
  it, no input satisfies the pin and the bid can never fill again unless a second form exists, which brings the choice back.
  Whether the script can read an input's outpoint at all was not checked. [assumption]
- **Per-buyer vault covenant** (one covenant per maker that every bid deposits into): it gives one UTXO per buyer, but the vault
  is an input shared by all of the maker's orders, so their fills conflict, and each bid either waits for it or has a second
  form.

## 15. Recommendation and decisions (replaces sections 11 and 12)

**Recommend the accumulating bid**, bundled with the next template re-version (ideally the last one before mainnet). It is the
only candidate with no filler choice, no new griefing and no stall. What it gives up: one UTXO per order instead of per buyer,
bought tokens wait in the order until it ends or the maker claims, and one bid fewer per 3 / 3 transaction.

Decisions for the founder:

1. Go or no-go, and timing (before mainnet, or with a later re-version).
2. Always accumulate, or a per-order `accumulate` flag set by the maker (proposed: a flag, on by default for GTC, ladder and DCA
   bids; off keeps today's behaviour for makers who want every fill in the wallet at once).
3. Claim: reuse the `cancel` entry (works, [engine F]) or add a `claim` entry that pins the continuation.
4. Accept that every later fill costs the filler 0.007 to 0.024 KAS more, paid from spread and tip.
5. Router: accept that x402 sells skip accumulating bids until a bid-custody leg is added, or schedule that leg with phase 1.
6. Scope: `KobBid` only, or `KobCondBid` too; pairs later.
7. Whether the wallet-side "merge while trading" is wanted as well for the UTXOs that remain (one per order).

Effort, accumulating bid (working days): contracts and catalogs 3 to 4; re-pin 2; builder 3; kobd 3 to 4; web and CLI 2 to 3;
audit pass, red-team rerun and soak 4 to 5. About 3.5 weeks.

Accumulating-bid experiments (Part F): lifecycle of three fills on four programs; G1 to G16 refused as described; two bids
against one resting ask accepted, a third refused by the token; refund, cancel and claim accepted, and a first fill of the
claimed order accepted. Whole file: 8 tests pass.

## 16. Chosen direction: ② default, ③ opt-in, ① compat

Founder direction, 2026-11-16:

- **② accumulate is the default** for a resting bid (section 13).
- **③ borrow top-up with a maker-paid tip is an opt-in** for bots and market makers (sections 1 to 9). Whoever wants
  consolidation prepays for it: the network through a later merge, or the filler through `topupTip` = the measured extra fee
  plus a small margin.
- **① today's new delivery stays** for compatibility (router sells, IOC / FOK, makers who want every fill in the wallet at once).

Prototype of all three in one template: `contracts/prototypes/borrow/KobBidUni.sil`. Experiments: Parts H and I of
`crates/kob-tests/tests/design/borrow_topup_experiments.rs`, [engine] on `KCC20Ref` and `KCC20PublicMint` (sizes and fees also
on `KForgeToken7214` and `KaspaRocketToken11458`). Whole file: 12 tests pass (Part J, option A for every borrow scheme, was added after this section).

### 16.1 One template with a delivery mode

State fields appended to today's `KobBid`: `mode` (0 new, 1 accumulate, 2 top-up), `recvThreshold`, `topupTip`, and `bought`
(mutable, last). `fill(nb, custIn, t, form)`; `form` matters only in mode 2 (0 plain new, 1 new receive UTXO, 2 top-up).
The mode is the maker's and immutable, so a filler cannot move an order between modes: a custody delivery on a mode-0 or
mode-2 order, a plain or top-up delivery on a mode-1 order, a receive-state delivery on a mode-0 order and a top-up that takes
more than `topupTip` are all refused. [engine H]

| Template | Script | What it serves |
|---|---|---|
| `KobBid` (shipped) | 1,629 B | ① |
| `KobBidAcc` | 2,251 B | ② |
| `KobBidTopup` | 1,943 B | ① ③ |
| `KobBidUni` | 2,679 B | ① ② ③ |

Fee per fill, one template against the separate templates (KAS, the same on all four programs because only the bid script
differs):

| Fill | One template | Separate template | Difference |
|---|---|---|---|
| mode 0, new delivery | today + 0.0021 | today (`KobBid`) | +0.0021 |
| mode 1, first or later fill | `KobBidAcc` + 0.0009 | `KobBidAcc` | +0.0009 |
| mode 2, top-up | `KobBidTopup` + 0.0015 | `KobBidTopup` | +0.0015 |

- **Can mode 2's rules sit behind a branch so that mode-0/1 fills do not pay for them? No.** They do sit behind a branch and
  are never executed for those orders, but the relay fee of these transactions is set by size, not by executed script
  (transient mass 13,084 against compute mass 11,012 for a mode-0 fill on `KCC20Ref`), and every fill reveals the whole script.
  Mode 2's rules are about 430 B, 0.0009 KAS on every fill of the template.
- **Recommendation: one template.** The 0.0009 KAS is 0.009% of a 10 KAS minimum fill. In return there is one bid kind for the
  payload, the indexer, the matcher and the trigger evidence of the conditional orders, which pin the bid template by hash;
  separate templates would make the stops accept two or three bid templates or leave opt-in bids out of the evidence.
- The alternative, if the 0.0009 KAS matters more: `KobBid` = modes 0 and 1, and a separate opt-in kind for mode 2 that is
  not trigger evidence.

Tip default for mode 2 (the measured extra fee of a top-up over a new delivery, times 1.25):

| Program | Extra fee | `topupTip` default |
|---|---|---|
| `KCC20Ref` | 0.00595 | 0.0074 |
| `KCC20PublicMint` | 0.00819 | 0.0102 |
| `KForgeToken7214` | 0.01455 | 0.0182 |
| `KaspaRocketToken11458` | 0.02304 | 0.0288 |

A filler prefers the top-up while the fee rate is below 1.25 times the relay floor; above it the filler delivers new (form 0
or 1) and keeps no tip. The maker can set a higher tip.

### 16.2 Mixed batch

One transaction that tops up a mode-2 bid and fills a mode-1 bid into its custody, against one resting ask, validates.
[engine H]

| Token inputs (3) | Token outputs (3) |
|---|---|
| receive UTXO of the mode-2 maker (leader, borrow path) | receive UTXO + n (first token output) |
| custody of the mode-1 bid (delegate, covenant-owned) | custody + n |
| ask custody (delegate, covenant-owned) | ask rest |

- One layout rule: the top-up bid must be the token's lowest-index bid, because its delivery has to be the leader's first
  output. With the mode-1 bid at the lower index the token program refuses and every order accepts. [engine H]
- A 3 / 3 program therefore carries at most one top-up plus one accumulating bid against one ask, or two accumulating bids
  (section 13.4). First fills of mode-1 bids and mode-0 fills use no token input.

### 16.3 Claim, then sell or merge, in one transaction

The maker signs once; the bid is spent through its `cancel` entry. [engine I]

| Shape | Token slots | Relay fee `KCC20Ref` / `KCC20PublicMint` |
|---|---|---|
| Bid ends; custody + 2 wallet UTXOs become the custody of one new `KobAsk` | 3 in / 1 out | 0.0240 / 0.0307 |
| Bid continues (`bought = 0`); custody + 1 wallet UTXO: `KobAsk` custody and change to the maker | 2 in / 2 out | 0.0185 / 0.0229 |
| Bid continues; custody + 2 wallet UTXOs merged into one plain UTXO | 3 in / 1 out | 0.0240 / 0.0307 |
| Custody + 3 wallet UTXOs | 4 in | refused by the 3 / 3 program |

- The ask placed this way is an ordinary ask: a taker bought its whole custody in the next transaction. [engine I1b]
- A continuing bid needs one more delivery carrier from the maker for its next custody. [engine I2, I3]
- The same transaction signed by another key is refused. [engine I1x]
- **Pre-sign check:** not run. It can verify the shape with what it already does for cancel-replace, which is also a cancel
  and a placement in one transaction (`docs/spec/matcher.md` section 9, `cancelReplace.ask`): every input re-read from the
  node, the bid's custody equal to its `bought`, the ask's placement record and custody output as for any placement
  (`docs/spec/kob1-payload.md:236-250`), the rest back to the maker. It needs a new signing-plan kind for "claim and place"
  and "claim and merge". [source, second-hand]

### 16.4 What follows for the plan

- Contracts: `KobBid` becomes the one-template form. Option A (section 6) ships in any case, for every borrow scheme; mode 2 depends on it.
- Wallet default (superseded 2026-11-17: mode 0 is the default, see bid-delivery-spec §10): mode 1 for GTC, ladder and DCA bids; mode 0 for IOC / FOK and market orders; mode 2 behind an advanced
  setting with the tip default above.
- Executor: sections 5 (mode 2) and 13.5 (mode 1) both apply; the one new ordering rule is 16.2.
- Router sells reach mode-0 bids and mode-2 bids (form 0); they skip mode-1 bids until the router gets a bid-custody leg.
- Effort: about 6 weeks for all three modes; about 3.5 weeks if mode 2 ships in a second step, which then needs one more
  template re-version unless its code is already in the template.

Open decisions:

1. One template (proposed) or a separate opt-in kind for mode 2.
2. If mode 2 ships later, whether its code goes into the template now (no second re-version, but it must be audited now).
3. Claim through the `cancel` entry (works) or a dedicated `claim` entry.
4. Router bid-custody leg with phase 1 or later.
5. The tip margin (proposed 1.25 times the measured extra fee) and the threshold value (section 4).

## 17. Implementation plan

Status: plan only, 2026-11-16. No production code exists. Founder decisions taken (2026-11-16): one template with delivery
mode 0 / 1 / 2 and mode 2's code in it now; claim through the existing `cancel` entry; router bid-custody leg in scope; tip
default 1.25 × the measured extra fee; receive threshold worth about 5 KAS per token per wallet; scope = every template that
delivers tokens to a buyer; option A ships first and separately (another agent, not in this plan); the template change is
bundled with the last re-version before mainnet.

Labels as in the rest of the document. Everything marked [engine] refers to the prototypes of sections 9, 13 and 16; everything
else in this section is a specification to be built and tested, not a result.

### 17.1 Final contract specification

**Common to every delivering template (the "delivery block").** New state fields, appended after the template's last
immutable field, in this order:

| Field | Type | Mutable | Meaning |
|---|---|---|---|
| `mode` | int | no | 0 new, 1 accumulate, 2 top-up. Any other value: every fill is refused |
| `recvThreshold` | int | no | mode 2: smallest threshold of a receive UTXO the maker accepts; must be > 0 for forms 1 and 2 |
| `topupTip` | int | no | mode 2: sompi a top-up fill leaves to the filler; `0 <= topupTip <= deliveryCarrier` |
| `bought` | int | **yes**, last field | mode 1: base units held in the order's bought-custody; 0 in modes 0 and 2 |

The fill entry gains one last argument `form` (read in mode 2 only). The first push stays the 8-byte `nb`.

| Mode | Form | Output at the delivery slot | Token input named by the fill | KAS |
|---|---|---|---|---|
| 0 | any | new plain UTXO of `n`, maker's key | any input of the token (template source) | carrier onto the delivery (today) |
| 1 | any | order continues: custody of `bought + n` (owner = order id, scheme `0x04`, borrow disabled). Order ends: plain UTXO of `bought + n`, maker's key | `bought == 0`: any input of the token. `bought > 0`: the custody (P2SH + exact state) | carrier onto the custody at the first fill only; later nothing but the price leaves the escrow |
| 2 | 0 | new plain UTXO of `n` | any input of the token | carrier onto the delivery |
| 2 | 1 | new receive UTXO of `n`: maker's key, borrow scheme `0x01`, guard = `recvThreshold` LE ‖ 24 zero bytes | any input of the token | carrier onto the delivery |
| 2 | 2 | the receive UTXO's state with `amount + n` | the maker's receive UTXO: P2SH, owner = maker, scheme `0x00`, borrow `0x01`, threshold `>= recvThreshold`, canonical guard, the order's commitment | the escrow releases `topupTip` (to anyone); output keeps at least the receive UTXO's KAS |

Rules shared by all of them [engine H for `KobBid`]:

- `bought` changes only in mode 1, by `+ n`, spliced into the continuation (script equality). Modes 0 and 2 require `bought == 0`.
- Stray guard: every token input of the order's token owned by the order id is refused, except the authenticated bought-custody
  when `bought > 0` (and, for ask-side kinds, the sell custody as today).
- The order never ends with a bought-custody behind: the ending fill, the refund / zero-fill and the IOC end release it whole,
  with its KAS, to the maker's key. Refund: at the bought-custody's own input index (as an IOC return today).
- `canContinue` reserves one `deliveryCarrier` per remaining fill in modes 0 and 2, and only until the custody exists in mode 1.
- Claim = the existing `cancel(sig)` entry. No new entry. Evidence that this is enough: `cancel` only checks the maker's
  SIGHASH_ALL signature (`KobBid.sil:281-286`), an in-place amend already continues a bid under its id that way
  (`crates/kob-protocol/src/build.rs:618-640` [source]), and claim, claim-and-place and claim-and-merge validate [engine F, I].
  No safety reason for a dedicated entry was found: a dedicated entry could only pin outputs the maker's signature already
  covers, and it would add bytes to every fill.

**Per template.**

| Template | Fill entry (new signature) | Delivery slot | Notes |
|---|---|---|---|
| `KobBid` | `fill(nb, tokIn, t, form)`; `refund(custIn)` | output `i` | reference implementation: `contracts/prototypes/borrow/KobBidUni.sil` (2,679 B) |
| `KobCondBid` | `settle(nb, tokIn, leg, ev, t, form)`; `refund(custIn)` | output `i` | already splices `stopPrice`, `amountLeft`, `armed` (`kob_v2_buy_tests.rs:1570-1576`); `bought` is a fourth window. Its booked-exit merge path (negative `nb`) is unchanged |
| `KobIfdAsk` | unchanged entries | its exit is a `KobCondBid` genesis | the entry state carries `mode`, `recvThreshold`, `topupTip` for the exit and writes them into the exit state with `bought = 0`; the exit-state length constant (321 B today) grows by 36 |
| `KobIfdBid` | unchanged behaviour | exit custody (covenant-owned) | re-pin only (it inlines the evidence hashes) |
| `KobCondAsk`, `KobAsk` | unchanged behaviour | — | `KobCondAsk` re-pin only; `KobAsk` only if its bytes change (they should not) |
| `KobPair` | `settle(nb, custIn, tTplIn, t, sOut, tOut, form)` | T at output `i` | KCC-20 T only: modes 0 / 1 / 2. A second custody (the bought T) next to the sell custody; the stray scan of T skips it. KRON T: mode must be 0 |
| `KobCondPair` | `settle(..., form)` | T at output `i` | as `KobPair` |
| `KobIfdPair` | `fill(...)` unchanged | exit genesis | carries the three fields for its `KobCondPair` exit, as `KobIfdAsk` does |
| Router (`kob_router.ag`) | `TokenToKas` sell legs and `TokenSwap` bid legs | delivery `k` | per bid leg the pinned delivery follows the bid's state: mode 1 → custody of `bid.bought + n` owned by the bid's id, with the bid's custody as an observed token input when `bid.bought > 0`; modes 0 and 2 → plain `n` to the maker (form 0), as today |
| KRON twins | — | — | sources unchanged. Mode 1 would be possible for KRON later; not in this re-version |

**Positional and ordering rules (builder and planner must produce them; the contracts and the token enforce them).**

1. Delivery at the order's own input index `i` (unchanged). Refund of a terminal bid at `M - 1 - i` (unchanged).
2. Mode 1: the bought-custody may sit at any token input position (it is a delegate or the leader, authorised by the order's
   input). Its refund return sits at its own input index.
3. Mode 2 form 2: the receive UTXO is the token's first input (leader, borrow path) and the top-up order is the token's
   lowest-index delivering order, so its delivery is the leader's first output. At most one top-up per token per transaction.
   [engine H2]
4. If a token has a refund-carrying terminal bid in the batch (those must be the first inputs), only that bid can be the
   top-up leg; otherwise no top-up for that token in that transaction.
5. Slots per token per transaction: mode 0 fill = 0 inputs / 1 output; mode 1 = 1 / 1 once the custody exists, 0 / 1 before;
   mode 2 top-up = 1 / 1. On a 3 / 3 program: two mode-1 bids, or one top-up plus one mode-1 bid, against one ask. [engine F, H2]
6. Router legs: the lock is the leader as today; a mode-1 bid's custody is one more token input. On 3 / 3: the lock plus at most
   two bid custodies, and deliveries plus change at most three outputs.

**Budgets and sizes.** `KobBid` grows from 1,629 B to about 2,680 B (+0.0021 KAS on a mode-0 fill) [engine H]. The other
templates grow by a similar block (estimate +0.9 to +1.1 KB each; measured in WP-C2 to C4). Compute budgets and keeper tips are
regenerated (`KOB_REGEN=1`), with new leader roles `{program}.leader.borrow.i{n}.o{m}`. The refund of a mode-1 bid now reveals
the token program: `refundTip` defaults are re-measured.

**Wallet rules (normative for the web app and the CLI).**

- Default mode: 1 for GTC bids (limit, ladder, DCA, stop-limit buys, pair buys of a KCC-20 token); 0 for IOC / FOK / market
  and for any KRON delivery; 2 only behind an advanced setting.
- `topupTip` default = `ceil(1.25 × extra)`, where `extra` is the program's measured relay-fee difference between a top-up and
  a new delivery, from a generated table next to `keeper_tips.json` (today's measurements: 0.00595 / 0.00819 / 0.01455 /
  0.02304 KAS for `KCC20Ref` / `KCC20PublicMint` / `KForgeToken7214` / `KaspaRocketToken11458`), capped at `deliveryCarrier`.
- Receive threshold, exact rule. `RECV_THRESHOLD_SOMPI = 500,000,000` (5 KAS). The first time a wallet places a mode-2 order
  for a token: `T = max(1, ceil(RECV_THRESHOLD_SOMPI × scale / price))` with the order's limit price (pair orders: the KAS-book
  price of the bought token used for `default_min_fill_pair`), stored per (wallet, token, extension commitment) and reused for
  every later order. An order's `recvThreshold = min(T, minFill - 1)`. If that is 0 (a `minFill` of 1), mode 2 is not offered
  for that order. The stored `T` is never changed silently; the settings page can reset it.

### 17.2 Work packages

Each package is one agent in its own worktree off the integration branch `reversion/delivery-modes`, and touches only the
files listed. "After" names the packages whose merge it needs. Option A (builders, web, CLI, decoder for borrow schemes 1 to 3)
is a separate change on master and is assumed merged before WP-B2, WP-W1 and WP-L1 start.

| WP | What | Files | After | Tests it must add |
|---|---|---|---|---|
| S0 | Spec freeze: this section turned into normative text; state layouts and offsets of every changed template fixed in a table | `docs/spec/order-types.md`, `docs/spec/kob1-payload.md` (state lengths), `docs/design/borrow-topup.md` | — | — |
| C1 | `KobBid` | `contracts/orders/KobBid.sil`, `KobBid.ctor.json`, new `crates/kob-tests/tests/kob/kob_bid_modes_tests.rs`, `tools/ablation/catalogs` | S0 | sections 9, 13, 16 cases as a catalog on 4 programs; ablation of every new `require` |
| C2 | `KobCondBid`, `KobIfdAsk` exit fields | `contracts/orders/KobCondBid.sil`, `KobIfdAsk.sil`, their ctor files, `crates/kob-tests/tests/kob/kob_v2_buy_tests.rs` | S0 | modes on settle; stop arm / trail / booked-exit merge unchanged; exit inherits the fields; splice windows |
| C3 | Pairs | `contracts/orders/KobPair.sil`, `KobCondPair.sil`, `KobIfdPair.sil`, ctor files, `crates/kob-tests/tests/common/pair_harness.rs`, `tests/kob/kob_pair*_tests.rs`, `kob_cond_pair_tests.rs`, `kob_ifd_pair_tests.rs`, `tests/audit/audit_pair_sec_tests.rs` | S0 | two custodies and the stray scan; mode 2 with T's leader; KRON T refuses modes 1 / 2; netting shapes |
| C4 | Router bid-custody leg | `contracts/argent/kob_router.ag`, `contracts/argent/KOBOrders*`, `contracts/argent/router/`, `tools/router-gen`, `crates/kob-tests/tests/argent/argent_router_tests.rs`, `tests/audit/c4_security_tests.rs` | S0 (layout), C1 for the final handle | sell into a mode-1 bid with and without custody; a custody stand-in; P13 bounds; first task: can Argent observe an optional input, or are per-shape entries needed [assumption] |
| P1 | Re-pin (one agent, serial) | `scripts/build-contracts.sh`, `scripts/build-argent.sh`, `contracts/artifacts/`, `contracts/SHA256SUMS`, `crates/kob-protocol/src/artifacts.rs` (`PINNED`), `crates/kob-protocol/data/retired/`, `scripts/extract-retired.mjs`, `crates/kob-protocol/src/retired.rs`, `crates/kob-tests/tests/kob/retired_cancel_tests.rs`, `artifacts_tests.rs`, `registry_tests.rs`, `docs/spec/template-retirement.md` | C1 to C4 | retired cancel of every outgoing template; pins recomputed |
| B1 | State codec and payload | `crates/kob-protocol/src/state.rs`, `state/pair.rs`, `payload.rs`, `router.rs` (intent / handle layouts), `crates/kob-wasm/src/lib.rs`, `web/src/kob/types.ts` | S0 | codec round trips, mutable windows, payload doc example, `check_new_order` terms (mode, tip cap, threshold) |
| B2 | Builder, KAS-book kinds | `crates/kob-protocol/src/tx.rs` (borrow witness, roles, seal ordering), `script.rs`, `build.rs` (bid and cond-bid legs, layout rules 2 to 5, refund with custody), `budget.rs`, `data/compute_budgets.json`, `data/keeper_tips.json`, new `data/topup_extra.json`, `tests/builders.rs`, `bid_refund.rs`, `budget_table.rs`, `cost_table.rs`, `carrier_fees.rs`, `golden.rs` + vectors | P1, B1 | every shape built and run in the engine on every program; refusals of rule 3 / 4 violations |
| B3 | Builder, pairs | `crates/kob-protocol/src/build/pair.rs`, `tests/pair_orders.rs`, `data/pair_keeper_tips.json` | P1, B1 | pair shapes per mode; routed fills |
| B4 | Builder, router | `crates/kob-protocol/src/build/intent.rs`, `router.rs`, `data/router_sil_abi.json`, `tests/intent_builders.rs`, `route_families.rs` | P1, B1 | executions into mode 0 / 1 / 2 bids; `execution_fee_mass_bound` |
| B5 | Claim plans | `crates/kob-protocol/src/build.rs` (cancel path only: claim, claim-and-place, claim-and-merge), `tx.rs` (`SigPlan` kinds), `tests/amend.rs`, `sweep.rs` | B2 (same file) | the three shapes on 3 / 3 and larger programs; slot refusals |
| K1 | Indexer | `crates/kobd/src/indexer/processor.rs`, `schema.sql`, `record.rs`, `packed.rs`, `reads.rs`, `book.rs`, `storage.rs` (migration), `crates/kobd/src/api/`, `tests/flow_*.rs`, `custody_exactness.rs`, `storage_migrate.rs` | B1 | bought-custody learned from fills and reverted on reorg; fill price per mode; `bought` in order views; holdings of receive UTXOs |
| K2 | Matcher | `crates/kobd/src/matcher/candidate.rs`, `batch.rs`, `planner.rs`, `lower.rs`, `chain.rs`, `book.rs`, `run.rs`, `submit.rs`, `engine.rs`, `pair.rs`, `tests/matcher_*.rs`, `c6_planner_props.rs`, `planner_fuzz.rs`, `submit_conflict_backoff.rs` | B2, B3, K1 | slot accounting; rule 3 / 4; top-up chosen only when the tip covers the fee; conflict on a receive UTXO → rebuild with form 1; chaining of custody and receive successors |
| K3 | Keepers and maintenance | `crates/kobd/src/keepers/mod.rs`, `maintenance.rs`, `x402/`, `tests/keepers.rs`, `keeper_liveness.rs`, `x402_*.rs` | B2, B4, K1 | refund with custody; router execution into a mode-1 bid |
| X1 | x402 | `crates/kob-x402/src/swap.rs`, `intent.rs`, `client/`, `docs/spec/x402-swap-and-pay.md` | B4 | swap verification with a custody delivery; vectors |
| L1 | CLI | `crates/kob-cli/src/trade/place.rs`, `plan.rs`, `plan_pair.rs`, `actions.rs`, `views.rs`, `opts.rs`, `order.rs`, `REFERENCE.md` | B2, B3, B5 | `--mode`, claim, claim-and-sell, refund view; e2e plans |
| W1 | Web: placement | `web/src/kob/orders/simple.ts`, `cond-common.ts`, `cond-ifd.ts`, `pair-simple.ts`, `pair-cond.ts`, `pair-ifd.ts`, `common.ts`, `programs.generated.ts`, ticket UI, settings | B1 (wasm) | mode defaults, tip and threshold rules, economics vectors |
| W2 | Web: holdings and claim | `web/src/kob/balances.ts`, `cancel.ts`, `merge.ts`, `positions.ts`, `web/src/ui/orders/*` (balances, order row, claim dialogs), `data/token-tracker.ts` | B5, K1 | "bought, held by the order" as the buyer's; claim, claim-and-sell, claim-and-merge flows |
| W3 | Web: pre-sign and signer | `web/src/kob/decode.ts`, `plan-types.ts`, `plan.ts`, `web/src/signer/summary.ts`, `core.ts`, their tests | B5 | new plan kinds; custody must go to the maker or into the placed ask; mutation tests |
| D1 | Docs | `docs/spec/matcher.md`, `docs/ops/executor.md`, `docs/api-reference.md`, `docs/user-guide.md`, `docs/security/AUDIT-2026-10.md` (P2, P3, P10, P13 wording) | any time after S0 | — |

**Parallel lanes without file conflicts.**

- Lane 1: S0 → C1 → P1 → B2 → B5.
- Lane 2: C2 (after S0) ; then K1 (after B1).
- Lane 3: C3 (after S0) ; then B3 (after P1, B1) .
- Lane 4: C4 (after S0, finished after C1) ; then B4, then X1.
- Lane 5: B1 (after S0) ; then W1 ; D1 alongside.
- After B2 / B3 / K1: K2 and K3 ; after B5 / K1: W2, W3, L1 in parallel (different directories).

Known shared files and the rule for them: `crates/kob-protocol/src/build.rs` (B2 then B5, never together);
`crates/kob-tests/Cargo.toml` (each contract package adds only its own `[[test]]` entry; the integrator resolves);
`kob_v2_buy_tests.rs` (C2 only); `tx.rs` (B2 only, B5 adds `SigPlan` kinds after B2 is merged).

### 17.3 Gates

1. **Per package:** its tests green under the heavy-job lock; touched suites only, CI runs the rest.
2. **After P1:** `cargo test --workspace`, `cd web && npm run gen:programs && npx vitest run`; `scripts/check-pins.mjs`;
   `template_generation` changed and recorded.
3. **Ablation:** `tools/ablation` over the KCC-20, pair and router catalogs on `KCC20Ref`, `KCC20PublicMint`,
   `KForgeToken7214`, `KaspaRocketToken11458`; every new check confirmed, `RESULTS.md` updated.
4. **Audit pass** over P1 to P14 with the changed wording: P2 (bids and pairs may hold a bought-custody, same exactness), P3
   (stray scan skips exactly the authenticated custodies), P10 (`topupTip` bounded like the carriers), P13 (router legs).
5. **Red-team rerun:** the borrow, family, output and input packs (`BOR-01`, `FAM-10`, `OUT-04`, `IN-02`) plus new attempts:
   custody stand-in, look-alike receive state, cross-mode delivery, tip over-take, claim by a non-maker.
6. **Retirement:** outgoing templates in the cancel-only table; `retired_cancel_tests` green; `docs/spec/template-retirement.md`
   row added.
7. **Deploy records:** `scripts/build-deploy.sh` for testnet-10, then mainnet only after the soak.

### 17.4 Testnet-10 soak

Never on the third-party node at 65.108.107.30. One executor of the new generation, one indexer, the web app on the test
host.

| Day | What runs | Pass condition |
|---|---|---|
| 1 | migration: old-generation orders cancelled through the cancel-only path; new generation placed | every old order cancellable; `template_generation` as released |
| 1 to 3 | bots place mode-1 ladders and DCA bids on 2 tokens of `KCC20PublicMint` and one of `KaspaRocketToken11458`; random takers | one custody per order; no order ends with a custody behind; indexer `bought` equals the chain |
| 2 to 4 | mode-2 makers with receive UTXOs; a second executor competes | holdings converge to one receive UTXO per maker and token; conflicts fall back to a new delivery without stalling |
| 3 to 5 | claim, claim-and-sell, claim-and-merge from the web app with KasWare and the private-key signer; refunds by the keeper after expiry | pre-sign check passes its own transactions and blocks tampered ones |
| 4 to 6 | router sells (x402 swap-and-pay) into mode 0, 1 and 2 bids; pair orders in every mode | payer and merchant bounds hold; pair custodies exact |
| 6 to 7 | reorg and restart drills; red-team replay on the dump | no stuck order, no accounting drift |

Exit: zero fund-safety findings, executor profit accounting within the fee estimates, and the third-party wallet check of
section 7 done.

### 17.5 Effort

Agent-days per package (one agent, including its tests):

| Group | Packages | Days |
|---|---|---|
| Spec | S0 | 1 |
| Contracts | C1 2, C2 3, C3 5, C4 5 | 15 |
| Re-pin | P1 | 2 |
| Protocol | B1 2, B2 4, B3 3, B4 3, B5 2 | 14 |
| Executor | K1 4, K2 5, K3 2 | 11 |
| x402, CLI | X1 2, L1 2 | 4 |
| Web | W1 2, W2 3, W3 3 | 8 |
| Docs | D1 2 | 2 |
| Gates | ablation 2, audit pass 3, red-team 2 | 7 |
| **Total** | | **about 64 agent-days** |

With five lanes the critical path is S0 → C1 → P1 → B2 → K2 → gates, about 4 weeks of calendar time, plus the 7-day soak:
**about 5 weeks** if nothing slips. Pairs and the router are the largest and least certain parts (C3, C4, B3, B4: 16 days);
without them the total would be about 45 agent-days. The router estimate depends on the first task of C4.

### 17.6 Risks to watch during the build

- Pair orders with two custodies of different tokens and the stray scans of both: the largest new contract surface.
- Router entry count if Argent cannot observe an optional input (per-shape entries multiply the router's size).
- `build.rs` is shared by B2 and B5 and is large: keep them strictly serial.
- Script growth of every delivering template: about +0.002 KAS on each fill; re-measure `DEFAULT_MIN_FILL_SOMPI` reasoning.
- Mode 2 depends on option A being on master first.
