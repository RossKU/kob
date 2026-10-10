# KOB documentation

Everything here describes a **pre-audit** system that runs on testnet-10 only. Where a document
says "normative", it is normative for this repository's code, not for the Kaspa network.

## Specifications (`docs/spec/`)

| Document | What it defines |
|---|---|
| [order-types.md](spec/order-types.md) | The order types on a token's KAS book and on a token/token pair, how each maps to a contract kind, the amount and rounding rule, and what the user gets |
| [matcher.md](spec/matcher.md) | Normative rules for the indexer, matcher, keepers and wallets: order visibility, batch selection, pair orders (netting, routing, the price record), triggers (the touch rule and the pair evidence modes), trailing, if-done exits, chaining, wallet defaults |
| [kob1-payload.md](spec/kob1-payload.md) | The `KOB1` transaction payload and the placement record that makes an order visible |
| [x402-kcc20-profile.md](spec/x402-kcc20-profile.md) | Proposal: a KCC-20 payment profile for the Kaspa x402 `exact` binding |
| [x402-swap-and-pay.md](spec/x402-swap-and-pay.md) | Proposal: swap-and-pay, paying a merchant in one token from another token or KAS |
| [x402-retry.md](spec/x402-retry.md) | Retrying an x402 payment (re-send, rebuild around the anchor input, stop) without paying twice |
| [kcc-conformance.md](spec/kcc-conformance.md) | Conformance with the KCC base specs (KCC-1, KCC-2, KCC-20): pinned upstream vectors, results, mismatches fixed, what still depends on upstream |
| [template-retirement.md](spec/template-retirement.md) | Templates this build does not pin: unsupported, ended by their maker with a raw cancel transaction |

## Operations (`docs/ops/`)

| Document | What it covers |
|---|---|
| [ops/executor.md](ops/executor.md) | Running `kob-executor`: one process (`run`), the indexer (node, storage, read API, runbook) and the matcher and keepers (keys, economics, flags) |
| [ops/indexer.md](ops/indexer.md), [ops/matcher.md](ops/matcher.md) | Pointers into `executor.md` (the roles are one binary) |

## Argent

| Document | What it covers |
|---|---|
| [argent.md](argent.md) | KOB as an Argent app: published artifacts, importing KOB orders from another Argent app, the swap-and-pay router |
| [argent-feedback.md](argent-feedback.md) | Questions and findings for the Argent team, each with its status and where it can be reproduced |
| [../argent/UPSTREAM.md](../argent/UPSTREAM.md) | The pinned Argent commit, our two patches, how to rebuild `argentc` and update the pin |

## Component documentation

| Document | What it covers |
|---|---|
| [../contracts/README.md](../contracts/README.md) | Contract sources, artifacts, reproducible builds, deployment records |
| [../contracts/adapters/kron/README.md](../contracts/adapters/kron/README.md) | The KRON token adapters |
| [../registry/README.md](../registry/README.md) | The token registry: strict template list, open token list |
| [../web/README.md](../web/README.md) | The web UI, its mock stack and tests |
| [../packages/kob-x402/examples/paid-api/README.md](../packages/kob-x402/examples/paid-api/README.md) | Example paid API using the TypeScript x402 SDK |
| [../tools/ablation/README.md](../tools/ablation/README.md) | Contract mutation testing: what each check is for |
| [../tools/soak/README.md](../tools/soak/README.md) | The testnet market simulator |
| [../tools/wallet-gate/README.md](../tools/wallet-gate/README.md) | Checks that browser wallets can sign covenant inputs |

## Project files

[../README.md](../README.md), [../SECURITY.md](../SECURITY.md), [../CONTRIBUTING.md](../CONTRIBUTING.md),
[../NOTICE](../NOTICE), [../LICENSE](../LICENSE).
