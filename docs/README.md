# KOB documentation

Everything here describes a system in **open review**. There is no external audit and the specification is not final. Mainnet
launches as a canary, and testnet-10 is the test network. Where a document says "normative", it is normative for this repository's
code, not for the Kaspa network.

## Guides

| Document | What it covers |
|---|---|
| [user-guide.md](user-guide.md) | Trading in the web app. It covers connecting KasWare, Kaspire or the private-key option, buying and selling, limit and market orders, partial fills, cancel and refund, and merging token UTXOs. It also covers the token fact badges, where token names and icons come from (kascov.io) and the icon switch, the current status of the bell (node and indexer), fees, and recovering a token you created |
| [faq.md](faq.md) | Questions people asked. Is it custodial, who signs what, front-running, settlement atomicity (and Miner Mode), and how market and limit orders work for a USD stablecoin when KAS moves |
| [api.md](api.md) | The read API, over HTTP and WebSocket with no key. It has the conventions, the endpoint table, examples against the public testnet-10 server, the WebSocket and health. [api-reference.md](api-reference.md) has every field, parameter and frame, the errors and limits, and the `[api]` configuration |
| [../crates/kob-cli/README.md](../crates/kob-cli/README.md) | The `kob` command line: trading, token issuance, recovery, scripts and bots. [REFERENCE.md](../crates/kob-cli/REFERENCE.md) has every option, the `--json` output and the exit codes |
| [development.md](development.md) | Developing KOB. It covers building and checking the contracts, the test suites, the web UI in mock mode, the `kobd` build features and the code style rules |
| [repository-map.md](repository-map.md) | What lives where in this repository |

## Specifications (`docs/spec/`)

| Document | What it defines |
|---|---|
| [order-types.md](spec/order-types.md) | The order types on a token's KAS book and on a token/token pair, and how each maps to a contract kind. It also gives the amount and rounding rule and what the user gets |
| [matcher.md](spec/matcher.md) | Normative rules for the indexer, matcher, keepers and wallets. They cover order visibility, batch selection, pair orders (netting, routing, the price record), triggers (the touch rule and the pair evidence modes), trailing, if-done exits, chaining and wallet defaults |
| [kob1-payload.md](spec/kob1-payload.md) | The `KOB1` transaction payload and the placement record that makes an order visible |
| [x402-kcc20-profile.md](spec/x402-kcc20-profile.md) | Proposal: a KCC-20 payment profile for the Kaspa x402 `exact` binding |
| [x402-swap-and-pay.md](spec/x402-swap-and-pay.md) | Proposal: swap-and-pay, paying a merchant in one token from another token or KAS |
| [x402-retry.md](spec/x402-retry.md) | Retrying an x402 payment without paying twice. The steps are re-send, rebuild around the anchor input, and stop |
| [kcc-conformance.md](spec/kcc-conformance.md) | Conformance with the KCC base specs (KCC-1, KCC-2, KCC-20). It gives the pinned upstream vectors, the results, the mismatches fixed, and what still depends on upstream |
| [token-programs.md](spec/token-programs.md) | Token-program profiles, with everything KOB needs per program in `crates/kob-protocol/data/programs.json`. It also covers onboarding the program of a launchpad with `kob registry add-program`, launchpad genesis shapes, the reviewer checklist, and KForge (the delivery id_type 0 order builds) |
| [template-retirement.md](spec/template-retirement.md) | Templates this build does not pin. They are unsupported, and their maker ends them with a raw cancel transaction |
| [token-metadata.md](spec/token-metadata.md) | Token metadata on chain under KCC-23 (pinned Draft). It covers the payload format, the KOB field convention and size limits, the two-transaction creation with its signing and resume, and launch proofs. It also covers how the indexer and the app read the facts of a token, and what happens when the registry and the chain disagree |

## Operations (`docs/ops/`)

| Document | What it covers |
|---|---|
| [ops/run-from-source.md](ops/run-from-source.md) | The short path from a clone to a running `kobd` on testnet-10. It covers the prerequisites (Windows, Linux), the build, an Indexer, an Executor and what runs where |
| [ops/executor-quickstart.md](ops/executor-quickstart.md) | Run an Indexer or an Executor from a source build or a release archive. It covers setup, node, funding, what to watch, withdraw, services, upgrade, Docker and Android. The short from-source path is [ops/run-from-source.md](ops/run-from-source.md) |
| [ops/executor.md](ops/executor.md) | Running `kobd` as one process (`kobd --executor`). Part B covers the Indexer: node, storage, read API and runbook. Part C covers the matcher and keepers of the Executor: keys, economics and flags |
| [ops/web-hosting.md](ops/web-hosting.md) | Hosting the web app on Cloudflare Pages. It covers the account, token, repository variables, the deploy gate, the token-names source (kascov.io: refresh policy, CSP, privacy), rollback and neutrality rules |
| [ops/chart-server.md](ops/chart-server.md) | A public read server, `kobd --indexer` behind Cloudflare Tunnel. It covers the setup order, tunnel, backups, alerts and upgrading without downtime |
| [ops/release.md](ops/release.md) | Cutting a mainnet release: gates, registry, reproducible build, hashes to publish, upgrade order and operator requirements |

## Security (`docs/security/`)

| Document | What it covers |
|---|---|
| [security/AUDIT-2026-10.md](security/AUDIT-2026-10.md) | The internal pre-mainnet security audit (canary gate). It covers invariants, findings, ablation, fuzzing, live testnet-10 attacks and residual risks |
| [security/REDTEAM-2026-11-kaspero.md](security/REDTEAM-2026-11-kaspero.md) | Third-party red team report by Kaspero Labs (kaspa-redteam, 2026-11-09). Consensus judged 23,874 hostile transactions and no bug was found. It is not an audit |

## Test reports (`docs/reports/`)

These are dated testnet-10 runs, kept as evidence. Their transaction ids are chain data, not build pins. Each report says what was
exercised, on which commit, and with what result.

| Document | What it covers |
|---|---|
| [reports/TN10-TEST-2026-10.md](reports/TN10-TEST-2026-10.md) | The first live testnet-10 roll (commit `8c1606c`). It covers every KAS-book order kind, x402, the registry checks, the live attacks on the pair orders (section 13) and the recovery drills of 2026-10-20 (section 14) |
| [reports/TN10-TEST-2026-10-14.md](reports/TN10-TEST-2026-10-14.md) | The roll onto `cf90250` |
| [reports/TN10-TEST-2026-10-15.md](reports/TN10-TEST-2026-10-15.md) | The roll onto `dadd20e` |

## Argent

| Document | What it covers |
|---|---|
| [argent/argent.md](argent/argent.md) | KOB as an Argent app: published artifacts, importing KOB orders from another Argent app, and the swap-and-pay router |
| [argent/argent-feedback.md](argent/argent-feedback.md) | Questions and findings for the Argent team, each with its status and where to reproduce it |
| [argent/argent-port-compare.md](argent/argent-port-compare.md) | `KobAsk` hand-written versus its Argent port, and where the size difference comes from |
| [../vendor/argent-upstream/UPSTREAM.md](../vendor/argent-upstream/UPSTREAM.md) | The pinned Argent commit, our two patches, how to rebuild `argentc` and how to update the pin |

## Component documentation

| Document | What it covers |
|---|---|
| [../contracts/README.md](../contracts/README.md) | Contract sources, artifacts, reproducible builds and deployment records |
| [../contracts/orders/kron/README.md](../contracts/orders/kron/README.md) | The KRON token adapters |
| [../contracts/prototypes/README.md](../contracts/prototypes/README.md), [../contracts/research/README.md](../contracts/research/README.md) | What is not deployed |
| [../registry/README.md](../registry/README.md) | The token registry, with its strict template list and open token list |
| [../web/README.md](../web/README.md) | The web UI, its mock stack and tests. [src/README.md](../web/src/README.md) is the module map |
| [../packages/kob-x402/examples/paid-api/README.md](../packages/kob-x402/examples/paid-api/README.md) | Example paid API using the TypeScript x402 SDK |
| [../tools/README.md](../tools/README.md) | The tools: the role of each and whether CI and the build scripts use it |
| [../tools/ablation/README.md](../tools/ablation/README.md) | Contract mutation testing, which shows which check rejects which attack. [RESULTS.md](../tools/ablation/RESULTS.md) has the latest runs |
| [../tools/soak/README.md](../tools/soak/README.md) | The testnet-10 market simulator: processes, bots, checker, operation and the latest run |
| [../tools/wallet-gate/README.md](../tools/wallet-gate/README.md) | Checks that browser wallets can sign covenant inputs |

## Project files

[../README.md](../README.md), [../SECURITY.md](../SECURITY.md), [../CONTRIBUTING.md](../CONTRIBUTING.md),
[../NOTICE](../NOTICE), [../LICENSE](../LICENSE).
