# Optional batch leader for KCC-20 (prototype)

Not audited, not used for issuance.

Holders stay on the 3 / 3 program. A token may commit at genesis to one batch-leader template, which moves many holder
UTXOs in one transaction.

| File | What |
|---|---|
| `KCC20Opt.sil` | Holder: [`KCC20Ref.sil`](../../../kcc20/KCC20Ref.sil) plus the leader commitment `B_TPL` / `B_PRE` / `B_SUF` (all zeros = none) |
| `KCC20Batch.sil` | Leader: amount 0, 16 slots; `batch`, `clone`, `retire` |
| `KCC20P2.sil` | Earlier form of `KCC20Opt`, kept as a measurement |

| | 3 / 3 | With leader commitment |
|---|---|---|
| Holder program | 2,915 B | 2,934 B |
| 1 -> 1 transfer | 0.00667 KAS | 0.00671 KAS |
| 70 UTXOs -> 1 | 35 transactions | 5 batches (16 slots), 1 batch (70 slots) |

Measured against rusty-kaspa v2.1.0 limits; details in [`kcc-conformance.md`](../../../../docs/spec/kcc-conformance.md) section 6.

```sh
cargo test -p kob-tests --test kcc20_opt_tests
cargo test -p kob-tests --test kcc20_conformance_tests
```
