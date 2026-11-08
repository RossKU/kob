<!-- The short path from a clone to a running kobd; the full operator guide is executor-quickstart.md. -->
# Run `kobd` on testnet-10 from source

`kobd` is one program with two roles. The **Indexer** (`kobd --indexer`) is keyless and read-only: it follows the chain, keeps
the order history and serves the read API the web app and bots use. The **Executor** (`kobd --executor`) adds the matcher and
the keepers: it fills crossing orders and refunds expired ones with a funded **hot key** and earns the spreads and tips. Building
from source is the supported route; tagged releases (`docs/ops/executor-quickstart.md`, section 1) can lag `master`. Testnet-10
coins have no value: start there.

**1. Prerequisites.** Git, [rustup](https://rustup.rs) (the pinned toolchain, Rust 1.94.0, installs itself on the first
build) and a C compiler (two dependencies, SQLite and secp256k1, compile C code). About 6 GB of disk for the build, a few GB
for an Indexer's data; no OpenSSL, protoc or clang.

* **Windows 10 / 11:** [Git for Windows](https://git-scm.com/download/win), rustup with its default MSVC toolchain, and the
  Visual Studio Build Tools with *Desktop development with C++* (the rustup installer offers them). Use PowerShell.
* **Linux:** `sudo apt install git curl build-essential` (or your distribution's equivalent), then rustup.

**2. Build.** The `deploy-tn10` build serves testnet-10 only and refuses any other network. The first build downloads
rusty-kaspa `v2.1.0` and takes some minutes.

```
git clone https://github.com/RossKU/kob.git
cd kob
cargo build --release --locked -p kobd --features deploy-tn10
```

The binary is `target/release/kobd` (`target\release\kobd.exe` on Windows); `kobd --version` prints `kobd 0.1.0`. Give it a
folder of its own, outside the repository and outside any folder a cloud service syncs: its configuration, key and data live
next to it.

```
# Linux                                         # Windows (PowerShell)
mkdir ~/kob-tn10                                mkdir C:\kob-tn10
cp target/release/kobd ~/kob-tn10/              copy target\release\kobd.exe C:\kob-tn10\
cd ~/kob-tn10                                   cd C:\kob-tn10
```

**3. An Indexer (no key, no funds).**

```
./kobd setup --non-interactive --node resolver --indexer     # writes kobd.toml and kob-tokens.json
./kobd --indexer                                               # runs it in the foreground (plain ./kobd does the same)
curl -s http://127.0.0.1:8090/v1/health                        # in a second terminal (Windows: curl.exe -s ...)
```

(On Windows write `.\kobd.exe` for `./kobd`.) `./kobd` with no configuration in its folder runs the same setup as a dialogue
instead (Enter takes each default). The health answer names `"network":"testnet-10"`, `"roles":["indexer"]` and a `state`
that goes `starting`, `catching_up`, `following`: the first start reads everything the node retains (about 36 hours of
chain, roughly 2.4 hours over public nodes) and `ok` turns `true` once the lag alarms clear. For the fast route (the books
from now on only), put `start = "sink"` at the top of `kobd.toml` (above `[launch]`) before the first start. A
public read server behind a tunnel: `docs/ops/chart-server.md`.

**4. An Executor (a funded hot key).** Give it a second folder with its own copy of `kobd` (for example `~/kob-tn10-exec`;
two processes cannot share a data directory) and run, in that folder:

```
./kobd setup --non-interactive --node resolver --executor --new-key     # prints the key's funding "address"
./kobd --executor --dry-run                                              # builds and checks every transaction, sends nothing
./kobd --executor                                                        # live (plain ./kobd does the same)
```

The setup generates `kob-operator.key` (64 hex characters, readable by your account only) and turns on lean storage and the
fast start: the Executor trades within seconds on the orders seen from its start while the history is indexed in the
background (`"book_complete":false` until then). Send the address at least 100 testnet KAS (200 recommended; mine them or ask
a faucet), as a few transfers of 10 KAS or more; `./kobd address-balance` shows the balance. Below 100 KAS it warns and keeps
trading. Let the dry run go until its status line says `indexer following` and `/v1/health` says `"roles":["executor"]`, stop
it, then start it live. The key is a hot key and KOB has had no external audit: keep only working capital on it, never more than you can
afford to lose, and back the file up where only you can read it.

**What runs where.**

| What | Where |
|---|---|
| Node | `rpc_url = "resolver"`: two public testnet-10 nodes found through the Kaspa resolvers (outbound HTTPS and WSS, port 443), one cross-checking the other. Your own node instead: `kaspad --testnet --netsuffix=10 --utxoindex --rpclisten-json=default` and `--node ws://127.0.0.1:18210` in the setup (required for Miner Mode; `docs/ops/executor-quickstart.md`, section 2) |
| Ports | the read API on `127.0.0.1:8090` (loopback; `--listen <addr>` or `[api] listen`). Nothing listens publicly and no inbound port is needed |
| Health | `GET /v1/health` (`ok`, `state`, `roles`, `network`, `book_complete`, lag), `GET /v1/health/ready` (200 / 503 for a probe); fields in `docs/api-reference.md` |
| Files | `kobd.toml` (the configuration; its relative paths are taken from its folder), `kob-tokens.json` (the supported token programs), `kob-operator.key` (Executor only), `kob-data/` (the SQLite store and, for the Indexer, the record log) |
| Logs | the console, with a status line every 10 seconds (`--status-secs`); `--log-file kobd.log` (or `log_file = "kobd.log"` in the configuration) also writes a rotated file. `RUST_LOG=debug` for more |
| Pause, stop | the file `kob-data/pause` pauses an Executor's trading; Ctrl+C stops either role cleanly; the next start continues where it stopped |

Next: services (Task Scheduler, systemd), Docker, upgrades and withdrawals in `docs/ops/executor-quickstart.md`; every flag,
the configuration keys, the economics and the runbook in `docs/ops/executor.md`. `kobd --help` lists the start options and
the commands, `kobd help <command>` a command's. The mainnet build is `--features deploy-mainnet`; mainnet is a canary
with no external audit (`docs/ops/release.md`).
