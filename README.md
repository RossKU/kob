# KOB: Kaspa Order Book

A permissionless limit order book for KCC-20 tokens on Kaspa layer 1. Every order is its own covenant UTXO: the covenant
enforces price, amount, custody and time in force on chain, and only the maker can cancel. Anyone can match crossing orders
and earn the spread. No bridge, no sequencer, no admin key, no oracle, no protocol fee.

> **Pre-audit.** KOB has had no external security audit. Testnet-10 is the network for testing; on mainnet use only funds
> you can afford to lose. See [SECURITY.md](SECURITY.md).

## Quick start

Trade: open the web app with KasWare or Kaspire, see the [user guide](docs/user-guide.md).

Run `kobd` (an Indexer, keyless; or an Executor that matches orders) on testnet-10, with Git, [rustup](https://rustup.rs)
and a C compiler installed:

```
git clone https://github.com/RossKU/kob.git && cd kob
cargo build --release --locked -p kob-executor --features deploy-tn10
mkdir ~/kob-tn10 && cp target/release/kobd ~/kob-tn10/ && cd ~/kob-tn10
./kobd setup --non-interactive --node resolver --indexer
./kobd
curl -s http://127.0.0.1:8090/v1/health
```

Windows, the Executor, ports, files and logs: [docs/ops/run-from-source.md](docs/ops/run-from-source.md). Services, Docker,
Android and upgrades: [docs/ops/executor-quickstart.md](docs/ops/executor-quickstart.md).

## Documentation

* [docs/README.md](docs/README.md): the index of every document
* [docs/api.md](docs/api.md): the read API; [crates/kob-cli](crates/kob-cli/README.md): the `kob` command line
* [docs/spec/](docs/spec/): order types, the matcher, the payload, token programs
* [docs/development.md](docs/development.md): build the contracts, run the tests, the web UI in mock mode;
  [docs/repository-map.md](docs/repository-map.md): what lives where

## Contributing, security, licence

[CONTRIBUTING.md](CONTRIBUTING.md). Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md). This is
experimental software that handles funds, provided as is, without warranty; the project operates no production service.

ISC, see [LICENSE](LICENSE). Third-party code keeps its own licence: [NOTICE](NOTICE).
