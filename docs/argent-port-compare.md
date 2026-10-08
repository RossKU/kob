# KobAsk: hand-written SilverScript vs an Argent port

A rough comparison, so the size difference is easy to check. I may well have misread something; corrections are welcome.

Both are compiled with the same constructor arguments:

* A: the hand-written order, [`contracts/v2/KobAsk.sil`](../contracts/v2/KobAsk.sil), 1,684 B
* C: an Argent port, [`port/kob_ask_port_idiomatic.ag`](../contracts/argent/port/kob_ask_port_idiomatic.ag)
  → [`KobAsk.idiomatic.sil`](../contracts/argent/port-out/KobAsk.idiomatic.sil), 2,705 B (+1,021)

Removing the rows below from each, A and C become the same 1,584 B script:

| | A | C |
|---|---:|---:|
| continuation check (A splices the one changed 8-byte field) | 79 | |
| own output-count checks | 21 | |
| `become`: state literal (20 fields) | | 591 |
| `become`: `validateOutputState` and `cont.length == count` | | 514 |
| generated count bounds, local, `cancel ... emits none` | | 16 |

So, as far as I can tell, most of the difference is `become` rebuilding and comparing the whole state, not duplicated checks.

Opcode listings with the byte range of each row:
[A](../contracts/argent/port/compare/KobAsk.hand.opcodes.txt),
[C](../contracts/argent/port/compare/KobAsk.idiomatic.opcodes.txt),
[sections](../contracts/argent/port/compare/KobAsk.sections.txt).

## A thought

If `become` compiled "self with only these fields changed" to a splice of those fields, the port would be close to A.
A hand-made sketch of that output, [`KobAsk.splice1.sil`](../contracts/argent/port/KobAsk.splice1.sil), is 1,713 B
(+29). It only seems sound for fixed-width fields, the same actor, and fields not reassigned before `become`. Just an idea.

## Notes

* `cancel ... emits none` refuses a cancel that re-creates the order under the same covenant id (our amend). A does
  not. In our tests (1,004 cases) this is the only behavioural difference.
* Regenerate: `KOB_WRITE_COMPARE=1 cargo test --release -p kob-tests --test argent_port_compare_tests`
  ([test](../crates/kob-tests/tests/argent_port_compare_tests.rs)).
