# Tools

| Tool | Role | Used by CI or build scripts | Documentation |
|---|---|---|---|
| `sil2argent` | Publishes the hand-written orders as an Argent app artifact and verifies it | Yes. `scripts/build-argent.sh` runs it. It is a workspace member and CI tests it | `src/main.rs`, [docs/argent/argent.md](../docs/argent/argent.md) |
| `router-gen` | Writes `contracts/argent/kob_router.ag` from `router_head.ag` | Yes. `scripts/build-argent.sh` runs it, and `router_head.ag` is a pin file of `scripts/check-pins.mjs` | `gen-router.sh`, [docs/argent/argent.md](../docs/argent/argent.md) |
| `ablation` | Contract mutation runner. It shows which `require` rejects which attack | No, run by hand | [README](ablation/README.md), [RESULTS](ablation/RESULTS.md) |
| `soak` | Testnet-10 market simulator with executors, bots, a checker and reports | No, run by hand | [README](soak/README.md) |
| `wallet-gate` | Checks that browser wallets can sign covenant inputs. It carries a v1 `BidOrder`, which is not deployed | Yes. `scripts/check-wallet-gate.mjs` runs it, and `build-contracts.sh --check` runs that script. CI runs `npm audit` on it | [README](wallet-gate/README.md), [RESULTS](wallet-gate/RESULTS.md) |
