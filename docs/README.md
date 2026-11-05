# KOB documentation

Everything here describes a **pre-audit** system (no external audit; mainnet launches as a canary, testnet-10 is the test network). Where a document
says "normative", it is normative for this repository's code, not for the Kaspa network.

## Guides

| Document | What it covers |
|---|---|
| [user-guide.md](user-guide.md) | Trading in the web app: connecting KasWare / Kaspire or the private-key option, buying and selling, limit and market orders, partial fills, cancel and refund, merging token UTXOs, the token fact badges, where token names and icons come from (kascov.io) and the icon switch, the node and indexer banners, fees, recovering a token you created |
| [faq.md](faq.md) | Questions people asked: custodial or not, who signs what, front-running, settlement atomicity (and Miner Mode), and how market and limit orders work for a USD stablecoin when KAS moves |
| [api.md](api.md) | The executor's read API: every HTTP endpoint and query parameter, response fields, `/v1/health`, the WebSocket messages, errors, limits and the `[api]` configuration |
| [../crates/kob-cli/README.md](../crates/kob-cli/README.md) | The `kob` command line: trading, token issuance, recovery; the `--json` output and exit codes for scripts and bots |
| [development.md](development.md) | Developing KOB: building and checking the contracts, the test suites, the web UI in mock mode, the `kobd` build features |
| [repository-map.md](repository-map.md) | What lives where in this repository |

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
| [token-programs.md](spec/token-programs.md) | Token-program profiles (everything KOB needs per program, in `crates/kob-protocol/data/programs.json`), onboarding a launchpad's program with `kob registry add-program`, launchpad genesis shapes, the reviewer checklist, KForge (the delivery id_type 0 order builds) |
| [template-retirement.md](spec/template-retirement.md) | Templates this build does not pin: unsupported, ended by their maker with a raw cancel transaction |
| [token-metadata.md](spec/token-metadata.md) | Token metadata on chain under KCC-23 (pinned Draft): the payload format, KOB's field convention and size limits, the two-transaction creation and its signing and resume, launch proofs, how the indexer and the app read a token's facts and what happens when the registry and the chain disagree |

## Operations (`docs/ops/`)

| Document | What it covers |
|---|---|
| [ops/run-from-source.md](ops/run-from-source.md) | The short path from a clone to a running `kobd` on testnet-10: prerequisites (Windows, Linux), build, an Indexer, an Executor, what runs where |
| [ops/executor-quickstart.md](ops/executor-quickstart.md) | Run an Indexer or an Executor from a source build or a release archive: setup, node, funding, what to watch, withdraw, services, upgrade, Docker, Android (the short from-source path is [ops/run-from-source.md](ops/run-from-source.md)) |
| [ops/executor.md](ops/executor.md) | Running `kobd`: one process (`run`), the Indexer (Part B: node, storage, read API, runbook) and the Executor's matcher and keepers (Part C: keys, economics, flags) |
| [ops/web-hosting.md](ops/web-hosting.md) | Hosting the web app on Cloudflare Pages: account, token, repository variables, the deploy gate, the token-names source (kascov.io: refresh policy, CSP, privacy), rollback, neutrality rules |
| [ops/chart-server.md](ops/chart-server.md) | A public read server (`kobd index` behind Cloudflare Tunnel): setup order, tunnel, backups, alerts, upgrading without downtime |
| [ops/release.md](ops/release.md) | Cutting a mainnet release: gates, registry, reproducible build, hashes to publish, upgrade order, operator requirements |

## Security (`docs/security/`)

| Document | What it covers |
|---|---|
| [security/AUDIT-2026-10.md](security/AUDIT-2026-10.md) | The internal pre-mainnet security audit (canary gate): invariants, findings, ablation, fuzzing, live testnet-10 attacks, residual risks |

## Test reports (`docs/testing/`)

Dated testnet-10 runs, kept as evidence (their transaction ids are chain data, not build pins): what was exercised, on which commit, with
what result.

| Document | What it covers |
|---|---|
| [testing/TN10-TEST-2026-10.md](testing/TN10-TEST-2026-10.md) | The first live testnet-10 roll (commit `8c1606c`): every KAS-book order kind, x402, the registry checks, the live attacks on the pair orders (section 13) and the recovery drills of 2026-10-20 (section 14) |
| [testing/TN10-TEST-2026-10-14.md](testing/TN10-TEST-2026-10-14.md) | The roll onto `cf90250` |
| [testing/TN10-TEST-2026-10-15.md](testing/TN10-TEST-2026-10-15.md) | The roll onto `dadd20e` |

## Argent

| Document | What it covers |
|---|---|
| [argent.md](argent.md) | KOB as an Argent app: published artifacts, importing KOB orders from another Argent app, the swap-and-pay router |
| [argent-feedback.md](argent-feedback.md) | Questions and findings for the Argent team, each with its status and where it can be reproduced |
| [argent-port-compare.md](argent-port-compare.md) | `KobAsk` hand-written versus its Argent port: where the size difference comes from |
| [../argent/UPSTREAM.md](../argent/UPSTREAM.md) | The pinned Argent commit, our two patches, how to rebuild `argentc` and update the pin |

## Component documentation

| Document | What it covers |
|---|---|
| [../contracts/README.md](../contracts/README.md) | Contract sources, artifacts, reproducible builds, deployment records |
| [../contracts/adapters/kron/README.md](../contracts/adapters/kron/README.md) | The KRON token adapters |
| [../registry/README.md](../registry/README.md) | The token registry: strict template list, open token list |
| [../web/README.md](../web/README.md) | The web UI, its mock stack and tests |
| [../packages/kob-x402/examples/paid-api/README.md](../packages/kob-x402/examples/paid-api/README.md) | Example paid API using the TypeScript x402 SDK |
| [../tools/ablation/README.md](../tools/ablation/README.md) | Contract mutation testing: which check rejects which attack ([RESULTS.md](../tools/ablation/RESULTS.md): the latest runs) |
| [../tools/soak/README.md](../tools/soak/README.md) | The testnet-10 market simulator: processes, bots, checker, operation, the latest run |
| [../tools/wallet-gate/README.md](../tools/wallet-gate/README.md) | Checks that browser wallets can sign covenant inputs |

## Project files

[../README.md](../README.md), [../SECURITY.md](../SECURITY.md), [../CONTRIBUTING.md](../CONTRIBUTING.md),
[../NOTICE](../NOTICE), [../LICENSE](../LICENSE).
