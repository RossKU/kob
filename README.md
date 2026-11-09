# KOB: Kaspa Order Book

A permissionless limit order book for KCC-20 tokens on Kaspa layer 1. Every order is its own covenant UTXO: the covenant
enforces price, amount, custody and time in force on chain, and only the maker can cancel. Anyone can match crossing orders
and earn the spread. No bridge, no sequencer, no admin key, no oracle, no protocol fee.

> **Open review.** KOB has had no external security audit. The code is public, the specification is not final and reports
> are welcome. Testnet-10 is the network for testing; on mainnet use only funds you can afford to lose. See
> [SECURITY.md](SECURITY.md).

## Quick start

Trade: open the web app, [kob.kaspa.ag](https://kob.kaspa.ag) (mainnet), or [kob-tn10.kaspa.ag](https://kob-tn10.kaspa.ag) (testnet-10, for testing; its fallback
mirror without Cloudflare and without private-key signing: [rossku.github.io/kob](https://rossku.github.io/kob/)), with KasWare or Kaspire, see
the [user guide](docs/user-guide.md).

Run `kobd` (an Indexer, keyless; or an Executor that matches orders) on testnet-10, with Git, [rustup](https://rustup.rs)
and a C compiler installed:

```
git clone https://github.com/RossKU/kob.git && cd kob
cargo build --release --locked -p kobd --features deploy-tn10
mkdir ~/kob-tn10 && cp target/release/kobd ~/kob-tn10/ && cd ~/kob-tn10
./kobd setup --non-interactive --node resolver --indexer
./kobd --indexer
curl -s http://127.0.0.1:8090/v1/health
```

Windows, the Executor, ports, files and logs: [docs/ops/run-from-source.md](docs/ops/run-from-source.md). Services, Docker,
Android and upgrades: [docs/ops/executor-quickstart.md](docs/ops/executor-quickstart.md).

## Bots and AI agents

The `kob` command line plans and signs exactly what the web wallet does, and every trading command prints one JSON document
(`--json`) with documented exit codes, so a script or an agent can read the book, place and cancel orders and follow fills:

```
cargo build --release --bin kob
kob book TUSD --network testnet-10 --json
kob place limit --token TUSD --side buy --amount 100TUSD --price 0.24KAS --network testnet-10 --key-file maker.key --dry-run --json
kob watch fills --token TUSD --network testnet-10 --json        # NDJSON, one event per line
```

Every command and kind: [crates/kob-cli](crates/kob-cli/README.md). The read API (HTTP and WebSocket, no key):
[docs/api.md](docs/api.md).

## Review scope

The live contracts are `contracts/orders/`; [contracts/README.md](contracts/README.md) states what is deployed, what to review, and what
is a prototype or third-party.

## Documentation

* [docs/README.md](docs/README.md): the index of every document
* [docs/spec/](docs/spec): order types, the matcher, the payload, token programs
* [docs/development.md](docs/development.md): build the contracts, run the tests, the web UI in mock mode;
  [docs/repository-map.md](docs/repository-map.md): what lives where

## Contributing, security, licence

[CONTRIBUTING.md](CONTRIBUTING.md). Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md). This is
experimental software that handles funds, provided as is, without warranty; the project operates no production service.

ISC, see [LICENSE](LICENSE). Third-party code keeps its own licence: [NOTICE](NOTICE).

KOB is built and run by volunteers. The donation address on the app's Support page is published here too, so you can check it: `kaspa:qq85cdj959gx33su28luhaymtq3yczx0jj40eufxwyxs77mk4vnpqd3m0nqqc` (real KAS on Kaspa
mainnet; a voluntary gift, with no return, no rights and no refund).
