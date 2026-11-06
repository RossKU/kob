# Research: NOT DEPLOYED

Nothing under `contracts/research/` is KOB production code or a KOB contract. Do not audit it as such: the deployed
order contracts are `contracts/orders/` and `contracts/orders/kron/` (see `contracts/README.md`, "What is deployed /
what to audit").

| Path | What |
|---|---|
| `argent/port/kob_ask_port.ag`, `kob_ask_port_idiomatic.ag` | `KobAsk` ported to Argent (1:1 and idiomatic), a size measurement (`docs/argent/argent-feedback.md`, item 10) |
| `argent/port-out/` | Their argentc output, written and checked by `scripts/build-argent.sh` |
| `argent/port/KobAsk.splice*.sil` | The idiomatic port with `become` lowered to a splice (`argent_become_splice_tests.rs`) |
| `argent/port/compare/` | Opcode listings and section sizes of the three variants (`docs/argent/argent-port-compare.md`), written and checked by `argent_port_compare_tests.rs` |

The `.sil` files here are generated, so they carry no "not deployed" header of their own. `scripts/build-contracts.sh`
does not compile them with silverc; it lists them in `contracts/SHA256SUMS`.
