# Lanes: what the indexer reads from a transaction outside the native lane

Since KIP-21 (Toccata), a transaction belongs to a lane: its subnetwork id. Each lane's accepted transactions are committed
separately in a chain block's `accepted_id_merkle_root`. The lanes are:

- the **native** lane (`00 × 20`);
- the **coinbase** lane (`01 00 × 19`);
- **user lanes** (`namespace(4) ‖ 00 × 16`, any namespace but the two above).

Consensus lets a user-lane transaction spend any UTXO whose script does not forbid it, and carry any payload
(`kaspa_consensus` `check_transaction_subnetwork`). A script can pin the lane of the transaction that spends it
(`OpTxSubnetId`). A KOB covenant output of a template that does not pin it can thus be spent on a user lane, and a
user-lane transaction can carry a KOB1 record. KOB's builders build native-lane transactions only.

This is the rule the indexer follows (`kobd`: `indexer::record::user_lane`, `indexer::trust`, `indexer::processor`,
`rpc::proof`). Other indexers must follow it too, or they disagree with `kobd` on what is open.

## The rule

1. **KOB state is created only by native-lane transactions.** An order, a continuation, a custody, a token UTXO of an
   order, a holding and a KCC-23 declaration are read from native-lane transactions only.
2. **A transaction on any other lane can only end what the index tracks.** When it spends a tracked output (an order UTXO,
   a token UTXO of an order, a holding), that output is closed. Nothing the transaction creates is tracked.
   - An order of a template that pins the native lane can only be spent there. Consensus refuses its spend on another
     lane, so it never meets this rule.
   - For anything else kobd tracks, a spend on another lane ends it in the index with the reason shown below. That covers
     orders of templates that do not pin the lane (earlier generations, on TN10 for instance), token custody and holdings,
     and locks until their templates are pinned too.
3. **KOB1 records are read on the native lane only.** A placement, an amend or a sweep on a user lane is not read.
4. **The coinbase lane is never read** (R9C1-M1): a coinbase carries consensus data, whatever its bytes.
5. **Chain proofs:**
   - The native lane is always proven.
   - A user lane is proven only when the batch shows, on that lane, a spend of an output the store tracks. That is
     decided from the store and the transaction's hashed fields (its inputs' outpoints), never from the node's word.
   - The transactions of any other user lane are neither folded nor proven.

## Why rule 5 is safe

- A lane the batch does not show at all is left to the node anyway. A proof is asked only for a lane the batch has a
  transaction in (`rpc::proof`, "Left to the node").
- So proving a shown lane whose transactions the index never reads adds no safety. A node that wants to hide a relevant
  transaction could hide the whole lane.
- Under rules 1 to 3, the only user-lane transactions the index reads are spends of tracked outputs, and their lanes are
  proven. So a node cannot invent such a spend, alter one, or drop one from a lane it shows.

## Why rules 1 to 3 (the cost of an attack)

- **Without the rule**, a flood of junk transactions, each on a lane of its own, cost every chain-proving indexer two lane
  proofs per lane. On a remote node that is tens of KAS an hour against hours of work (R9C1-H1, `docs/ops/api-bounds.md`).
- **If outputs created on a user lane were tracked**, one tracked token could seed a chain of user-lane transactions, each
  spending the last one's output on a fresh lane. Every link would then be "relevant" and proven, for a fee each.
- **Under the rule**, every proven user lane ends one tracked output that was created on the native lane beforehand. The
  work is bounded by those outputs, which cost storage mass and capital to make, and each of them is ended once.

## What a closed output shows

**An order** spent on a user lane:
- has the status `closed`, by a closing event of kind `unknown`;
- has its UTXO's `spent_entry` set to `other_lane`;
- shows `lane_end` on its view and on its maker's list (`GET /v1/orders/{id}`, `GET /v1/orders?maker=`):

```json
{"reason": "spent_on_other_lane", "txid": "<spending tx>", "lane": "02000007",
 "note": "spent by a transaction on another lane: a remainder may exist on chain",
 "remainder": [{"txid": "<spending tx>", "index": 0, "value": "100000000", "amount_left": "600000000", "state": "<hex>"}]}
```

- `remainder` lists every output of the spending transaction that carries the order's covenant id.
- `state` is the remainder's encoded state when the indexer proves it from the output's script (the same derivation as a
  native continuation), else `null`.
- The order's live custody, bought-custody and strays are closed with it, as at a gap.

**A holding or token UTXO** spent on a user lane is marked spent by that transaction (`spent_txid`).
- Tokens received on a user lane are not shown.
- They come back into view when their owner moves them on the native lane (to themselves, for instance): a native transfer
  proves the state of its outputs whatever the store knew of its inputs.

## How a maker takes a remainder back

This applies to an order whose template does not pin the native lane. Anyone can fill such a resting order on a user lane:
a real trade at the maker's price. The index then stops following the
order, and its remainder drops out of the book until the maker acts. The funds are not at risk: the remainder is the
maker's on chain, cancellable by the maker only, and fillable only on its own terms.

There are two ways back:
- **`kobd db import-orders`** with the remainder's state (a recovery file whose entry has `state` = `lane_end.remainder[i].state`
  and the order's covenant id). The index checks it against the node's UTXO set at that exact script and follows the order
  again: open, cancellable from the view, listed.
- **`kob recover --from <order view> --amount-left <covenant id>=<amount_left>`** finds the remainder on the node from
  `lane_end.remainder[i].amount_left`, and `--cancel` ends it. A remainder left by a fill of exactly `minFill` is found
  without the hint (its fallback search).

**Bound of the griefing.** One minimal fill per order (`minFill` at the maker's price) takes an order out of the book
until its maker re-imports it. Per round, the griefer pays that trade and its fees, and the maker loses only the time the
order is not listed.

A stale row makes nobody sign anything:
- The closed order is in no book, no matcher plans it, and no keeper acts on it.
- A taker who held an old view and builds against it spends an outpoint that is gone, which the network refuses.
