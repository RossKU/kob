# KobAsk: hand-written vs idiomatic Argent port, opcode by opcode

A is the hand-written order [`contracts/v2/KobAsk.sil`](../contracts/v2/KobAsk.sil); C is argentc's output
[`port-out/KobAsk.idiomatic.sil`](../contracts/argent/port-out/KobAsk.idiomatic.sil) for
[`port/kob_ask_port_idiomatic.ag`](../contracts/argent/port/kob_ask_port_idiomatic.ag) (the 1:1 port of
[argent-feedback.md](argent-feedback.md) item 10 without the body checks the generated ones repeat). Both are compiled
with the same constructor arguments. Each opcode is mapped to its source statement by the compiler's debug info (the
bytecode is identical to a compile without it). Each row below is removed from the source in turn and the script
compiled again; the byte ranges are the offsets in the full script of the opcodes that disappear. With all rows
removed, A and C are the same 1,584-byte script.

| Section | A | C | Byte ranges |
|---|---:|---:|---|
| everything else (identical in A and C) | 1,584 | 1,584 | |
| continuation check `scriptPubKey == contSpk(amountLeft - n)` (8-byte splice) | 79 | | A [1214, 1276), [1324, 1339) |
| own output-count checks `OpCovOutputCount(selfId) == 1` / `== 0` (x2) | 21 | | A [889, 896), [1202, 1209), [1350, 1357) |
| `become`: `cont.length == count`, `validateOutputState` | | 514 | C [2036, 2547), [2617, 2620) |
| `become`: state literal (`State[] cont`, `cont.append(State { 20 fields })`) | | 591 | C [1283, 1599) literal, [821, 841) declaration; 11 more runs, 232 B (moving the 20 field arrays at branch ends, picks that become shorter opcodes); 99 stack-depth pushes 27 B shorter, 4 B new |
| generated count bounds `0 <= count <= 1` | | 8 | C [320, 328) |
| generated local `gen__next_output_count = OpAuthOutputCount(...)` | | 3 | C [318, 320), [2596, 2597) |
| generated `cancel ... emits none`: `OpAuthOutputCount == 0` | | 5 | C [2639, 2644) |
| total | 1,684 | 2,705 (+1,021) | |

Entries: A `settle` [287, 1605), `cancel` [1615, 1680); C `settle` [287, 2621), `cancel` [2631, 2701). The state
fields are [1, 277) in both; `amountLeft`'s value is [236, 244). Item 10's table is measured on the 1:1 port, where the
literal is 593 B and the continuation check 81 B (different stack depths around them).

Listings (offset, size, opcode, push data, source line, section markers):
[hand](../contracts/argent/port/compare/KobAsk.hand.opcodes.txt),
[idiomatic](../contracts/argent/port/compare/KobAsk.idiomatic.opcodes.txt),
[splice1](../contracts/argent/port/compare/KobAsk.splice1.opcodes.txt),
[per-section table](../contracts/argent/port/compare/KobAsk.sections.txt). Regenerate (written by
`crates/kob-tests/tests/argent_port_compare_tests.rs`; without the variable the test fails if a file is stale):

```
KOB_WRITE_COMPARE=1 cargo test --release -p kob-tests --test argent_port_compare_tests
scripts/build-argent.sh             # C (contracts/argent/port-out/)
KOB_WRITE_SPLICE=1 cargo test --release -p kob-tests --test argent_become_splice_tests   # D1
```

**Behavioural difference.** `cancel ... emits none` compiles to `OpAuthOutputCount == 0`, so C refuses a cancel whose
transaction re-creates the order under its covenant id (KOB's amend, `build_amend_order`); A's cancel checks only the
maker's `SIGHASH_ALL` signature. In `argent_become_splice_tests.rs` (1,004 positive and negative cases) C and D1
decide 26 cases differently from A, all of them such a cancel (the amend fixtures among them), and no other case.

## Proposal

D1, [`port/KobAsk.splice1.sil`](../contracts/argent/port/KobAsk.splice1.sil), is C with `become` compiled as a splice
of the changed fixed-width fields: the literal is reduced to the fields not written as `field: field` (here
`amountLeft - n`), and `validateOutputState` becomes `scriptPubKey == P2SH(this redeem script with [236, 244) replaced
by amountLeft - n as byte[8])`. The count bounds, `len == count` and cancel's check stay. D1 is 1,713 B, **+29 B**
over A (splice check 73 B and slot 40 B against A's 79 B and 21 B, plus the generated bounds, local and cancel check,
16 B); with those removed it is the same 1,584 B script. Its verdicts differ from A's in the same 26 cases as C's. It is
sound when:

* every changed field has a fixed width, so its bytes are at a compile-time offset of the state span;
* the continuation is the same actor and template as the spent script (the splice copies the spent script's own code,
  which P2SH binds to the spent output);
* a field written `field: field` is not reassigned in the entry before `become` (otherwise it is not unchanged).

`KobAsk.splice1.sil` is a reference for what the compiler could emit; it is produced by a text transform of C in
`argent_become_splice_tests.rs`.
