# KOB: Kaspa Order Book

A permissionless limit order book for KCC-20 tokens on Kaspa layer 1. Every order is its own covenant UTXO. The covenant enforces
price, amount, custody and time in force on chain, and only the maker can cancel. Anyone can match crossing orders and earn the
spread. There is no bridge, no sequencer, no admin key, no oracle and no protocol fee.

> **Open review.** KOB has had no external security audit. The code is public, the specification is not final and reports
> are welcome. Testnet-10 is the network for testing. On mainnet, use only funds you can afford to lose. See
> [SECURITY.md](SECURITY.md).

## Quick start

To trade, open the testnet-10 web app, [kob-tn10.kaspa.ag](https://kob-tn10.kaspa.ag), with KasWare or Kaspire. Its tokens are test
tokens with no value. See the [user guide](docs/user-guide.md).

[kob.kaspa.ag](https://kob.kaspa.ag) serves the same testnet-10 app today: both hosts are one deployment, and kob-tn10.kaspa.ag is an alias of it.
Mainnet is not launched. At launch, kob.kaspa.ag opens on mainnet by default and the header network switch is turned on; testnet-10 stays one click away.

To run `kobd` on testnet-10, install Git, [rustup](https://rustup.rs) and a C compiler. `kobd` is either an Indexer, which is
keyless, or an Executor, which matches orders.

```
git clone https://github.com/RossKU/kob.git && cd kob
cargo build --release --locked -p kobd --features deploy-tn10
mkdir ~/kob-tn10 && cp target/release/kobd ~/kob-tn10/ && cd ~/kob-tn10
./kobd setup --non-interactive --node resolver --indexer
./kobd --indexer
curl -s http://127.0.0.1:8090/v1/health
```

- Windows, the Executor, ports, files and logs: [docs/ops/run-from-source.md](docs/ops/run-from-source.md).
- Services, Docker, Android and upgrades: [docs/ops/executor-quickstart.md](docs/ops/executor-quickstart.md).

## Bots and AI agents

The `kob` command line plans and signs exactly what the web wallet does. Every trading command prints one JSON document (`--json`)
with documented exit codes. A script or an agent can read the book, place and cancel orders and follow fills.

```
cargo build --release --bin kob
kob book TUSD --network testnet-10 --json
kob place limit --token TUSD --side buy --amount 100TUSD --price 0.24KAS --network testnet-10 --key-file maker.key --dry-run --json
kob watch fills --token TUSD --network testnet-10 --json        # NDJSON, one event per line
```

- Every command and kind: [crates/kob-cli](crates/kob-cli/README.md).
- The read API, over HTTP and WebSocket with no key: [docs/api.md](docs/api.md).

## Review scope

The live contracts are in `contracts/orders/`. [contracts/README.md](contracts/README.md) states what is deployed, what to review,
and what is a prototype or third-party.

## Documentation

* [docs/README.md](docs/README.md): the index of every document
* [docs/spec/](docs/spec): order types, the matcher, the payload, token programs
* [docs/development.md](docs/development.md): build the contracts, run the tests, the web UI in mock mode
* [docs/repository-map.md](docs/repository-map.md): what lives where

## Contributing, security, licence

See [CONTRIBUTING.md](CONTRIBUTING.md). Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md).

This is experimental software that handles funds. It is provided as is, without warranty, and the project operates no production
service.

ISC, see [LICENSE](LICENSE). Third-party code keeps its own licence, see [NOTICE](NOTICE).

KOB is built and run by volunteers. The donation address on the app's Support page is published here too, so you can check it:
`kaspa:qq85cdj959gx33su28luhaymtq3yczx0jj40eufxwyxs77mk4vnpqd3m0nqqc`. It is a real KAS address on Kaspa mainnet. A donation is a
voluntary gift, with no return, no rights and no refund.
