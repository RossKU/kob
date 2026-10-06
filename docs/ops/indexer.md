# KOB indexer: operations guide

Moved. The indexer, the matcher and the keepers are one binary, `kob-executor`, and one operator
guide: **[executor.md](executor.md)**.

* Running the indexer, node, resources, storage, read API, monitoring, runbook, backups, record log
  format and known limits: [executor.md, Part B](executor.md#part-b-the-indexer) (sections 1 to 11,
  numbering unchanged, e.g. the gap runbook is 7.4).
* `kob-executor index` runs the indexer alone; `kob-executor run` runs it together with the matcher
  and the keepers over one store ([Part A](executor.md#part-a-one-process-kob-executor-run)).

Protocol v3 (no lots) changes that matter to an indexer operator (details in Part B):

* **Schema 5, no migration from older databases.** Quantities are token base units and prices sompi per whole token
  (`orders.scale`, `min_fill`, `tip`, `budget_rate`, `initial_amount`; `order_state.filled_amount`, `remaining_amount`,
  `amount_exact`; `order_events.amount`; `order_utxos.spent_amount`). A database written before v3 (its `orders` table has the
  old `lot_units` column) is refused untouched by every writer and reader: move it aside and run `kob-executor index replay`,
  which rebuilds it from the record log.
* **Old record logs still read.** Every frame decodes; reveals of the retired templates (format-1 frames, which all predate v3,
  and format-2 frames naming a retired template hash, `indexer::layouts`, the fourteen v2.6 templates included) are dropped and
  counted. Placement records of payload versions 2 and 3 (orders of retired templates) never become orders: each is a reject
  `retired_template:<kind>`.
* **Listing.** `non_standard_scale` (an order of a token with `decimals` must use `10^decimals`, at most `10^9`) replaces the
  standard-lot rule; `min_order_value_sompi` (default 1 KAS: the order's amount at its quote, a bid's escrow) replaces the minimum
  lot value (`order_value_below_minimum`). An allowlist entry's `lot_size` key is ignored.
* **API.** Every view counts base units and quotes per whole token; the renamed fields are listed in Part B section 5.

Protocol v2.6 changes that matter to an indexer operator (details in Part B):

* **No receipts.** The trade receipt is retired: there is no `receipts` table, no `/v1/receipts`
  route, no `[receipts]` configuration (an old configuration's `[receipts]` table is ignored) and
  no receipt genesis to watch. The database schema is version 3: opening a version-2 database drops
  its `receipts` table and indexes in place (nothing else referred to them; no replay needed). A
  record log written by a v2.4 build still reads: its receipt reveals (template code `0x07` /
  `0x87`) are skipped. The `deploy-tn10` build only records its network (it refuses another one).
* **Trigger evidence in events.** A stop arms, ratchets or fills at its trigger only in a
  transaction that also fills a plain resting `KobAsk` / `KobBid` (its evidence). The `arm` and
  `trail` events, and the `fill` event of a triggered stop leg or stop entry, carry
  `detail.evidence = {"input": k, "order": "<evidence order covenant id>", "side": 1|2, "price": "<quote>"}`
  (`side` 1: a resting ask was filled, 2: a resting bid); the evidence's own fill is an ordinary
  `fill` event of that order in the same transaction.
