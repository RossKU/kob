# Tools

| Tool | Role | Used by CI or build scripts | Documentation |
|---|---|---|---|
| `sil2argent` | Publishes the hand-written orders as an Argent app artifact and verifies it | Yes: `scripts/build-argent.sh`; a workspace member, tested in CI | `src/main.rs`, [docs/argent/argent.md](../docs/argent/argent.md) |
| `router-gen` | Writes `contracts/argent/kob_router.ag` from `router_head.ag` | Yes: `scripts/build-argent.sh`; `router_head.ag` is a pin file of `scripts/check-pins.mjs` | `gen-router.sh`, [docs/argent/argent.md](../docs/argent/argent.md) |
| `ablation` | Contract mutation runner: which `require` rejects which attack | No: run by hand | [README](ablation/README.md), [RESULTS](ablation/RESULTS.md) |
| `soak` | Testnet-10 market simulator: executors, bots, checker, reports | No: run by hand | [README](soak/README.md) |
| `wallet-gate` | Checks that browser wallets can sign covenant inputs (carries a v1 `BidOrder`, not deployed) | Yes: `scripts/check-wallet-gate.mjs` (run by `build-contracts.sh --check`); CI runs `npm audit` on it | [README](wallet-gate/README.md), [RESULTS](wallet-gate/RESULTS.md) |
