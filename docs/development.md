# Develop and test

## Requirements

- The prerequisites of [ops/run-from-source.md](ops/run-from-source.md).
- The `wasm32-unknown-unknown` target. `rust-toolchain.toml` adds it.
- A POSIX shell for the scripts. On Windows, use Git Bash.
- Node 22 or newer for the web UI and the tools.

## Build the contracts

The committed artifacts must reproduce byte for byte.

```
scripts/build-contracts.sh --check                # silverc from vendor/silverscript, argentc from the pinned upstream
scripts/build-contracts.sh --check --no-argent    # only the SilverScript artifacts
scripts/build-contracts.sh                        # rewrite the artifacts after editing a .sil
```

## Run the tests

```
KOB_SKIP_NETWORK_TESTS=1 cargo test --workspace --locked
scripts/build-wasm.sh --install-bindgen --test               # wasm bindings + node golden-vector tests
node tools/ablation/ablate.mjs --list --suite all            # contract mutation catalogue (see tools/ablation/README.md)
```

Without `KOB_SKIP_NETWORK_TESTS=1`, some `kobd` tests contact a testnet-10 node and mine their own funds. Do that only on purpose.
[CONTRIBUTING.md](../CONTRIBUTING.md) lists the lint and the other checks CI runs.

## Run the web UI in mock mode

Mock mode needs no node, no wallet and no network.

```
scripts/build-wasm.sh --install-bindgen --web --slim
cd web
npm ci
npm run fetch-sdk                          # the official rusty-kaspa v2.1.0 wasm SDK, not the npm package
npm run build:wasm -- --copy
npm run mock -- --history --fund           # mock indexer + node on http://127.0.0.1:8790
npm run dev                                # then Settings: indexer URL and node URL = http://127.0.0.1:8790
```

The web app defaults to mainnet, no indexer and the public resolver of the SDK. A real run needs your own settings and a browser
wallet, see `web/README.md`. On mainnet the app is a canary with no external audit. Use only funds you can afford to lose.

## Builds of `kobd`

- `--features deploy-tn10` records the network of `contracts/deploy/testnet-10` and refuses any other. See
  [ops/run-from-source.md](ops/run-from-source.md).
  - Its templates are the reference ones. No template depends on a network, and there is no genesis step.
- The reference build has no feature and needs `--network` on every command.
- `--features deploy-mainnet` pins `registry/tokens.json`.
  - The release checklist, the hashes to publish, the upgrade order and the operator requirements are in `docs/ops/release.md`.
- A token list of your own goes in `tokens_path`.
  - `registry/tokens.example.json` is a format example with fictional testnet tokens. See `registry/README.md`.

## Code style

These rules apply to new and changed code and comments. A change that only applies them is a commit of its own.

All languages:

1. **Wrap comments by hand.** Use 100 columns in Rust and TypeScript, and 80 in `.sil` and `.ag`. rustfmt does not wrap comments.
   Example: `// Refund. Due at the earliest of expiryDaa, 90 days idle and the IOC kill.`
2. **Plain sentences.** Start with a capital, end with a period and give one idea each. Do not build clause chains from colons,
   semicolons or nested parentheses. A list of three or more things is a bullet list.
   Example: `// The carrier is order state. Changing this default re-versions nothing.`
3. **Describe the code as it is.** Leave out dates, audit or test-run ids, people and history such as "was", "no longer" or "since v2".
   They belong in the commit body and in the reports and audit documents.
   Example: write `// 5 s.`, not `// 5 s, founder 2026-09-30`.
4. **Say why, not what.** Delete a comment that restates the next line. Move a rationale of more than about 6 lines to `docs/spec`
   and link to it from the comment.
   Example: `// Measurements: docs/spec/matcher.md, "Measured costs".`

Rust:

5. **Module docs.** Open `//!` with one sentence that names what the module is. Add at most a few short paragraphs under
   `# Headings`. Do not put tables of history or measurements there.
   Example: `//! Typed order and token states, and the order arithmetic.`
6. **Item docs.** Give every `pub` item a `///`. Its first sentence is at most about 100 characters and does not repeat the
   signature. Details follow an empty `///` line. Tag code fences ```` ```text ```` unless they are real doctests.
   Example: ``/// Value of `n` base units at `rate`, rounded in the maker's favour.``
7. **Names.** Give locals descriptive names, such as `token_input`, not `tin`. Use one-letter names only for indices, loop counters
   and one-line closures. A rename is a commit of its own and never touches a public, serialized or JSON name.
   Example: `let custody_amount = state.amount_left;`
8. **Size.** When you work on a function of more than about 150 lines or a file of more than about 1,500, split it. Do the split in a
   commit that changes nothing else.
   Example: `fn build_batch_pass` becomes `collect_legs`, `plan_outputs` and `price_and_fund`.
9. **Errors and panics.** Write errors as `thiserror` enums with lowercase messages and no final period. Do not call `unwrap()`
   outside tests. `expect("...")` states the invariant.
   Example: `.expect("slice is 32 bytes")`
10. **Logging.** Daemon code logs through `tracing` macros imported by name. `println!` and `eprintln!` are for command output only.
    Example: `use tracing::{info, warn};`

SilverScript and Argent:

11. **Shape.** A header of at most about 30 lines says what the contract is and lists its entries and state. Write each invariant as a
    short comment above the `require`s that enforce it. Proofs and long rationale live in `docs/spec`.
    Example: `// Stray guard. Settle refuses any other token input owned by this id.`
12. **Comment-only edits.** Change full-line comments and blank lines only. Never touch a code line, its indentation or its
    trailing comment. Never rename a contract, entry, parameter, state field, struct or constant. Every such commit passes
    `scripts/check-comment-only.cjs` and a rebuild with `scripts/build-contracts.sh` that leaves every artifact unchanged.
    Example: `node scripts/check-comment-only.cjs origin/master contracts/orders/KobAsk.sil`
13. **Generated files.** Edit a generated file through its generator, and say so on its first line.
    Example: `// Generated by tools/router-gen/gen-router.sh from router_head.ag. Do not edit.`

TypeScript:

14. **Docs.** Give exported functions and types a `/** */` whose first sentence says what the item does. Inline comments follow
    rules 1 to 4. A style commit changes no exported name, i18n key, `data-` attribute or CSP and Trusted Types code.
    Example: `/** Servers of one network, as config.json gives them. */`

Commits:

15. **Subject of at most 72 characters, `area: imperative summary`, then a blank line and a body wrapped at 72.** The body carries the
    reason, the measurements and the history that used to sit in comments. Make one logical change per commit.
    Example: `kob-protocol: plain module docs for state.rs`

Some tests and tools read source text and anchor on comment lines. The ablation catalogs in `tools/ablation/catalogs` do this, and
so do some contract tests in `crates/kob-tests`. Run `node tools/ablation/ablate.mjs --dry-run --suite all` after editing a comment
in a contract. It reports an `EDIT-ERROR` when an anchored line moved.
