# KOB protocol v3: red team report (Kaspero Labs, 2026-11-09)

> Third-party report by Kaspero Labs, published with their permission. The text below is theirs, converted from the
> original page ([REDTEAM-2026-11-kaspero.html](REDTEAM-2026-11-kaspero.html), rendered: [REDTEAM-2026-11-kaspero.png](REDTEAM-2026-11-kaspero.png)); the maintainer's notes are in the last
> section. A pass on this review is not an audit or a guarantee, and it is not an independent audit of KOB (`SECURITY.md`,
> [AUDIT-2026-10.md](AUDIT-2026-10.md)).

Adversarial testing of the KOB order and router covenants with kaspa-redteam.

| | |
|---|---|
| Prepared for | Ross (KOB) |
| Tool | kaspa-redteam 0.17.2, attack library 0.10.2 ([github.com/kasperolabs/kaspa-redteam](https://github.com/kasperolabs/kaspa-redteam)) |
| Judge | rusty-kaspa 2.1.0 consensus, in process |
| Date | Nov 9, 2026 (v2: with the 46 spent-coin txs) |

**Verdict: no bug found.** Across the contracts, the harness dump and the testnet transactions, 23,874 hostile
transactions were built and judged by consensus. 75 were accepted, and none of them is a bug: 60 are behaviour the
maintainer confirmed is by design (inside `max_extra`), and 15 are open only because the tool can't read that token
program yet.

| | |
|---|---|
| 23,874 | hostile transactions judged by consensus |
| 1,925 | real transactions attacked (1,811 harness, 114 testnet-10) |
| 75 | accepted attempts: 60 by design, 15 open, 0 bugs |
| 907 | accepted attempts matching a designed tip, budget or the signer's own money (reported as notes) |

## 1. What was tested

| Input | What it is | Attacked | Hostile txs judged | Accepted |
|---|---|---:|---:|---:|
| **Contracts** (production zip c9d545a6) | The order, router and KCC-20 sources, searched by the tool: it builds honest spends of every entry it can reach, then mutates them. 14 groups (each order with the files it depends on, the upstream issuers, four router actors). | 130 of 178 paths | 963 | 0 |
| **Harness dump** (kob-tx-dump/1, KOB 817fa73a) | 1,913 engine-built transactions over 12 token families: create, take, match, cond, ifd, rpt, pair, refund, cancel, amend, route, send, router intents. Keys of the taker, matcher and keepers given, so paths were attacked from those seats too. | 1,811 txs | 21,402 | 67 |
| **Testnet-10** (122 txids + 46 with spent coins) | Real accepted transactions with every coin they spent: 78 fetched from api-tn10, and 36 more from the spent-coin bundle (placements and merges whose parents are pruned). No keys (outsider's view). 22 have no covenant input (placements funded by plain coins only, a token creation, two native x402 payments). | 114 txs | 1,509 | 8 |
| **Total** | | | **23,874** | **75** |

The 102 dump transactions not attacked are creations funded by plain coins only: there is no covenant input to attack.
The order, KCC-20 and router programs in the transactions match the tool's compile of the zip by template hash (KobAsk,
KobBid, KobCondAsk, KobCondBid, KobPair, KCC20Ref, TokenToKas_sell recognized on testnet), so the sources tested are
what is deployed.

## 2. The attacks

Each attack takes an honest transaction, changes one thing a covenant should forbid, and asks consensus. Accepted means
the change goes through on chain. An accepted change that only moves a tip the covenant names, its spender's budget, or
the signer's own money is reported as a note, not a finding.

| Attack | What it tries | Judged | Notes | Findings |
|---|---|---:|---:|---:|
| `OUT-01` | Send an output to the attacker instead of its recipient (one output, or everything) | 8,409 | 12 | 19 |
| `OUT-03` | Keep output 0, skim a later output | 5,209 | 38 | 43 |
| `IN-01` | Spend a second covenant coin next to the first (double-count one check) | 3,172 | 0 | 0 |
| `OUT-02` | Keep the recipient, cut the value to 10% | 1,586 | 8 | 2 |
| `FEE-01` | Halve every output, the rest becomes miner fee | 1,586 | 9 | 2 |
| `IN-02` | Move the covenant input to another index | 1,586 | 0 | 0 |
| `OUT-04` | Keep the payouts, take what they leave over | 1,165 | 840 | 9 |
| `BOR-01` | Make a coin the covenant holds borrowable and see if it bricks | 134 | 0 | 0 |
| `FAM-10` | Sweep a second token cell owned by the covenant to the attacker | 64 | 0 | 0 |
| All 33 attacks | Source search runs: signatures, secrets, state owner, outputs, fees, inputs, family and borrow packs | 963 | 0 | 0 |
| **Total** | | **23,874** | **907** | **75** |

Notes, by reason: within `refundTip` 756, within `keeperTip` 76, within `max_extra` 6, the fee out of the signer's own
share 2, paid from the signer's own coins 67. The notes column counts only the real transactions; the source search
produced none. Each note names the field it matched; a leftover above that field stays a finding.

## 3. Findings

### Router filler output (by design)

OUT-01 and OUT-03 on the 0.18 to 0.2 KAS output to the x402 payer in KasToToken buys and in x402 intent executions:
whoever executes can redirect or shorten it.

Maintainer's answer: it is a filler that holds the output index open, not pinned on purpose; the payer's loss stays
within `max_extra`.

34: OUT-01 19 (15 dump, 4 testnet), OUT-03 15 (dump).

### Delivered cell carrier (by design)

OUT-03 on the 1 KAS carrier of the token cell the router delivers (KasToToken, TokenSwap, TokenToKas and their Kron
versions): the executor can shorten it and keep the difference.

Maintainer's answer: `max_extra` is one cap, the payer's signed worst case; token amount, owner and extension are
pinned, the carrier's KAS is not.

26: OUT-03 (22 dump, 4 testnet).

### KForge-1785 refunds (open)

OUT-04 on 8 refunds and 1 close: the leftover is 0.028 KAS, exactly the request's `refundTip` (2,800,000), so almost
certainly the tip. But this family's order coins don't match the zip's order programs, so the tool can't read the field
and name it.

Question: which order build does the KForge-1785 family use? (Answered in the maintainer's notes below.)

9: OUT-04 (dump).

### KRON sends (open, tool side)

OUT-02, OUT-03 and FEE-01 on two KRON `send.tokens` transactions. The KRON cells are the taker's, but KRON isn't a known
program to the tool yet, so it can't see that the signer owns them and keeps the outputs protected.

Not a KOB question: KRON support is next on the tool's side (the 2,433 B and 2,732 B programs).

6: OUT-02 2, OUT-03 2, FEE-01 2 (dump).

### Worth a line in KOB's docs

Because `max_extra` is a single unearmarked cap, whoever executes a router intent can always keep up to `max_extra` of
the payer's KAS. That is safe as long as wallets set it tight; a high default hands executors a free margin. This is
about use, not the covenant. Done on KOB's side: the default `max_extra` is now computed from the intent's shape
(carrier + 0.14 KAS for a single ask, was + 1 KAS).

## 4. What this does not cover

- **Matcher transactions from the source alone.** Paths that need several orders in one transaction (a conditional's
  `update`, if-done exits, pair settles, router `buy` / `sell` legs) are not built by the search yet. 48 of 178 source
  paths stayed unexplored. The dump covers these shapes as real transactions; that is where they were tested.
- **Token-aware attacks** (FAM-10, BOR-01) ran only where the cell is the shipped KCC-20 reference build (198 attempts).
  The other builds, KRON included, were attacked through the generic input and output attacks only.
- **Signed paths on testnet** (29 of 114: cancels, an amend, one intent creation, and the placements and merges, which
  the maker signs for their own token cell) were tested as an outsider: without the key, only changes that break the
  signature. The dump's listed keys covered the taker, matcher and keeper seats.
- **Not in the testnet set:** a refund sent by the maker (keepers refund first; the payout is the maker's either way),
  and pair netting outside the soak (2 txs).
- **Consensus only.** The judge is rusty-kaspa's consensus validator; mempool relay policy (standardness, minimum fees)
  is not applied.

## 5. How it was run

| | |
|---|---|
| Contracts | `kaspa-redteam orders/KobCondAsk.sil orders/KobAsk.sil orders/KobBid.sil kcc20/KCC20Ref.sil --args orders/KobCondAsk.ctor.json`, and the same for each group |
| Harness dump | One baseline file per family and kind; keys = taker, matcher, keeper, keeper (intents). `kaspa-redteam orders/*.sil kcc20/KCC20Ref.sil argent/router/sil/*.sil --baseline-only --baseline <file>` (Kron families with `orders/kron/*.sil`; if-done txs again with their `ctor.json`) |
| Testnet-10 | `kaspa-redteam fetch <txid>:<kind> --network testnet-10`, then the same `--baseline-only` run. The spent-coin bundle was converted to the same baseline format; all 36 with a covenant input replay exactly as given. |

Every accepted attempt has a replayable file (`kaspa-redteam replay findings/OUT-03.json`) with the exact transaction
and its spent coins.

## Maintainer's notes (KOB)

- **KForge-1785 family.** Its orders use the KRON delivery-0 build (`KobAskKronD0` .. `KobIfdAskKronD0`): the same
  sources as `contracts/orders/kron/` with `TYPE_DELIVERY = 0x00` instead of `0x03`, for KRON programs whose holders are
  id_type 0, such as the legacy KForge program `KForgeToken1785` (testnet-10 only). The derived sources exist only in the
  build directory (`scripts/build-contracts.sh`, `VARIANTS`; one byte of each bytecode differs), so they are not among
  the zip's order programs. See `contracts/orders/kron/README.md` and `contracts/third-party/kforge/UPSTREAM.md`. The
  leftover of these refunds is the request's `refundTip`, as the report expects.
- **`max_extra`.** The single-cap property is documented in `docs/spec/x402-swap-and-pay.md` section 17.8 (whoever
  executes the intent may keep up to `max_extra` of the payer's KAS, so wallets should set it tight).
- **Since the report.** The order, router and KCC-20 sources have not changed since the tested baselines (zip c9d545a6,
  harness dump 817fa73a).
