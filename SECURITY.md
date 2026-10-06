# Security policy

## Status

KOB is **pre-audit software**. It has had internal review passes by the author and an
extensive engine-level test and ablation suite (see `tools/ablation/README.md`), but **no
independent third-party audit**. Nothing here is deployed to Kaspa mainnet, and the token
program templates in `registry/tokens.json` are `review_status: pending-review` except the two KRON
programs, `reviewed` after an internal review with per-token conditions (no token entry is listed yet). Do not put
funds you cannot afford to lose into these contracts, and do not treat a passing test suite as an
audit.

Testnet-10 is the only network the project runs on. Mainnet use is not recommended until an
independent review exists; this file will say so when that changes.

## Reporting a vulnerability

Please report suspected vulnerabilities **privately**, not in public issues:

* Use GitHub's private vulnerability reporting for this repository (Security tab, "Report a
  vulnerability").
* If that is unavailable, open a public issue that only says you have a security report and how
  to reach you, without any detail, and the maintainer will move the conversation to a private
  channel.

Helpful details: the affected file or contract (`contracts/v2/*.sil`, `crates/kob-protocol`,
`crates/kob-executor`, ...), a transaction or test that reproduces it (the harness in
`crates/kob-tests` runs contracts in the rusty-kaspa v2.1.0 script engine, which makes small
reproductions easy), and what an attacker gains.

This is a small, volunteer-run project: expect an acknowledgement within a few days rather
than hours. There is no bug bounty.

## Scope

In scope: the SilverScript order contracts, the Argent shell and router (`contracts/argent/`),
the Rust protocol library and executor (indexer, matcher, keepers, x402 facilitator), the
WebAssembly bindings and the web UI, and the specifications in `docs/spec/`.

Out of scope: vulnerabilities in third-party code that KOB only uses (rusty-kaspa, SilverScript,
Argent, token programs such as KaspaCom's KCC20 or the KRON programs, wallets); please report
those upstream. If KOB's use of them makes the problem worse, that part is in scope.

## Design notes that matter for reports

* The order contracts enforce prices, quantities, custody and triggers on chain. A matcher that
  misbehaves should only be able to build invalid or unprofitable transactions; a report that
  shows otherwise is the most valuable kind.
* There is no operator key and no admin function in the order contracts.
* The executor holds only an operator hot key (matcher and keepers); the x402 facilitator and
  the indexer hold none. The key is never taken from the command line: a key file or a systemd credential (an environment variable is supported but discouraged).

## Disclaimer

The software is provided as is, without warranty (see `LICENSE`). Using it on any network is at
your own risk.
