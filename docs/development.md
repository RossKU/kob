# Develop and test

Requirements: the prerequisites of [ops/run-from-source.md](ops/run-from-source.md), the `wasm32-unknown-unknown` target (`rust-toolchain.toml` adds it),
a POSIX shell (Git Bash on Windows) for the scripts, and Node 22 or newer for the web UI and tools.

**Build the contracts.** The committed artifacts must reproduce byte for byte:

```
scripts/build-contracts.sh --check                # silverc from vendor/silverscript, argentc from the pinned upstream
scripts/build-contracts.sh --check --no-argent    # only the SilverScript artifacts
scripts/build-contracts.sh                        # rewrite the artifacts after editing a .sil
```

**Run the tests.**

```
KOB_SKIP_NETWORK_TESTS=1 cargo test --workspace --locked
KOB_CARRIER_KAS=20 cargo test --locked -p kob-tests          # harness order carrier of 1 (the wallet default), 2, 10 (default) or 20 KAS
scripts/build-wasm.sh --install-bindgen --test               # wasm bindings + node golden-vector tests
node tools/ablation/ablate.mjs --list --suite all            # contract mutation catalogue (see tools/ablation/README.md)
```

Without `KOB_SKIP_NETWORK_TESTS=1` some `kobd` tests contact a testnet-10 node and mine
their own funds. Do that only on purpose. `CONTRIBUTING.md` lists the lint and the other checks CI runs.

**Run the web UI in mock mode** (no node, no wallet, no network):

```
scripts/build-wasm.sh --install-bindgen --web --slim
cd web
npm ci
npm run fetch-sdk                          # the official rusty-kaspa v2.1.0 wasm SDK, not the npm package
npm run build:wasm -- --copy
npm run mock -- --history --fund           # mock indexer + node on http://127.0.0.1:8790
npm run dev                                # then Settings: indexer URL and node URL = http://127.0.0.1:8790
```

The web app defaults to mainnet, no indexer and the SDK's public resolver. A real run needs
your own settings and a browser wallet; see `web/README.md`. On mainnet it is a pre-audit canary:
use only funds you can afford to lose.

**Builds of `kobd`.** `--features deploy-tn10` ([ops/run-from-source.md](ops/run-from-source.md)) records the network of `contracts/deploy/testnet-10` and refuses any
other; its templates are the reference ones (no template depends on a network, and there is no genesis step). The reference
build (no feature) needs `--network` on every command; `--features deploy-mainnet` pins `registry/tokens.json`, and its release
checklist, the hashes to publish, the upgrade order and the operator requirements are in `docs/ops/release.md`. A token list of
your own goes in `tokens_path` (`registry/tokens.example.json` is a format example with fictional testnet tokens;
`registry/README.md`).

The lint and the other checks CI runs: [CONTRIBUTING.md](../CONTRIBUTING.md).
