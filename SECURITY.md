# Security policy

## Status

KOB is **pre-audit software**, and there is **no external security audit before launch** (a
maintainer decision, 2026-10-12). What exists: internal review passes by the author, the engine
test suites in `crates/kob-tests` (the contracts run in the real rusty-kaspa script engine), the
ablation (mutation) suites that show which check rejects which attack (`tools/ablation/README.md`)
and fuzz/soak tests. None of that is an independent audit, and a passing test suite is not one
either.

Mainnet launches as a **canary**, with a visible pre-audit notice in the app (a header badge, a
first-visit banner and a footer line on every page). **Use it at your own risk, and only with funds
you can afford to lose.**

Token programs (`registry/tokens.json`): the two KRON programs are `reviewed` after an internal
review whose per-token conditions (a genesis check, no live minter) are reported as the derived
warnings `genesis_unverified` and `live_mint_authority`; KOB lists no token. KCC-20 token
issuance is permissionless: tokens issued with KOB's standard 3 / 3 reference program or with the
reference's published public-mint build (KOB's second issuance option, also used by third-party
launchpads such as Kasseal) are tradable on mainnet with no listing or curation step, and being
tradable says nothing about a token's safety or genuineness (curation belongs to the community token
lists a user chooses, `registry/README.md`). The web app shows the facts instead of a verdict: wherever a token is shown (token header,
next to the order button, market list) it prints neutral badges for the indexer's `genesis_unverified` and `live_mint_authority` and for the program's
mint and blacklist powers, and a stop on a token whose genesis is not verified gets a one-line note next to the order button; none of them blocks an
order or asks for a click (the order flow has no acknowledgement boxes). Both programs (`kcc20-ref-3x3`, 2026-10-12, and
`kcc20-ref-public-mint`, 2026-10-13) are `reviewed` by internal review only; a public-mint token
with a live `PublicMint` can still grow up to its genesis allowance. The other KCC-20 programs (the
8 / 8 prototype, KaspaCom's) are `pending-review` and not tradable on mainnet.

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
