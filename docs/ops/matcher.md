# Running a KOB matcher and keeper

Moved. The matcher, the keepers and the indexer are one binary, `kob-executor`, and one operator
guide: **[executor.md](executor.md)**.

* Economics, keys and funds, the node, flags, systemd unit, monitoring, families and tests:
  [executor.md, Part C](executor.md#part-c-the-matcher-and-the-keepers).
* `kob-executor run` runs the matcher and the keepers in the indexer's process, planning against
  its store and following acceptance through its follower
  ([Part A](executor.md#part-a-one-process-kob-executor-run)); `match` and `keep` with
  `--book-file` stay for deployments that keep the key away from the public API.
* The normative rules are `docs/spec/matcher.md`.

Protocol v2.6 changes that matter to a matcher operator (details in Part C, "Stops: triggered and
armed in the batch"):

* **No receipts, no mints, no probes.** A stop triggers only in a transaction that also fills a plain
  resting `KobAsk` / `KobBid` (its evidence). The matcher fills a triggered stop next to its evidence
  and arms (or ratchets) the other stops that evidence serves with an `update`, whenever the batch
  stays profitable, taking their `keeperTip` (a priority fee, possibly 0); `--no-arm` turns the updates off. `--no-mint` and the `--probe*` flags are gone.
* **Keepers only refund, kill, close and sweep.** `keep --no-arm` / `--no-trail` (and `run`'s) are gone.
