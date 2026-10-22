# KOB executor: quick start for operators

Anyone can run a KOB executor: it follows the chain, lists the order book and, with a small funded hot key, matches
crossing orders and refunds expired ones, earning the spreads and the makers' tips (`docs/ops/executor.md`, Part C,
*Economics*). This page takes you from the download to a running executor. The full operations guide is
`docs/ops/executor.md`.

The executor talks to nothing but the Kaspa node you point it at (your own, on the same machine by default). It sends no
telemetry, charges no fee, does not update itself and connects to no server of the KOB project.

## 1. Download and check

Prebuilt binaries are attached to each [GitHub release](https://github.com/RossKU/kob/releases), one archive per network and
platform:

| Archive | For |
|---|---|
| `kob-executor-<version>-mainnet-x86_64-pc-windows-msvc.zip` | mainnet, Windows 10 / 11 (64-bit) |
| `kob-executor-<version>-mainnet-x86_64-unknown-linux-gnu.tar.gz` | mainnet, Linux x86-64 (glibc) |
| `kob-executor-<version>-testnet-10-...` | the same for testnet-10 |

A binary serves exactly one network (the deployment build, `docs/ops/release.md`): the mainnet binary refuses a testnet
node and the other way round. Start on testnet-10 if you have never run one.

Each archive holds `kob-executor` (the executor), `kob-cli` (the command-line client), this page, `LICENSE` and a
`SHA256SUMS` of its files; the release also lists a `SHA256SUMS` of every archive. Check the archive you downloaded against
it before you run anything:

```
# Windows (PowerShell)
Get-FileHash .\kob-executor-<version>-mainnet-x86_64-pc-windows-msvc.zip -Algorithm SHA256
# Linux
sha256sum -c --ignore-missing SHA256SUMS
```

The hash must equal the line of the release's `SHA256SUMS` for that file. The release notes also carry the hashes of
`scripts/release-hashes.sh` (`docs/ops/release.md`, 6): the order templates, the token registry and the binaries built
from the tagged commit. Anyone can rebuild the binary from that commit with the pinned toolchain
(`cargo build --release --locked -p kob-executor --features deploy-mainnet`) and compare.

Unzip it into a folder of its own that your account can write to (for example `C:\kob` or `~/kob`; not `Program Files`):
the setup writes its configuration, the key file and the database next to the program.

**Windows SmartScreen.** The Windows binary is not code-signed, so the first start shows "Windows protected your PC".
Click **More info**, check that the file is the one whose hash you verified, then **Run anyway**. Windows asks once per
file.

## 2. Run a Kaspa node

The executor needs a rusty-kaspa node (v2.1.0 or later) of the same network on the same machine, with its JSON wRPC
listener on and a UTXO index (the executor looks up its funds there):

```
# mainnet
kaspad --utxoindex --rpclisten-json=default --retention-period-days=7
# testnet-10
kaspad --testnet --netsuffix=10 --utxoindex --rpclisten-json=default --retention-period-days=7
```

`--rpclisten-json=default` listens on `127.0.0.1:18110` (mainnet) or `127.0.0.1:18210` (testnet-10), the addresses the
executor uses by default. Keep the RPC on loopback; never `--unsaferpc`. A new node needs some hours to sync; the executor
waits for it. Sizing and the reasons for each flag: `docs/ops/executor.md`, Part B 2 and 3.

## 3. Double-click

Start `kob-executor` (on Windows double-click `kob-executor.exe`; on Linux run `./kob-executor` in a terminal). Without a
configuration next to it, it runs the setup, which asks (Enter takes the default):

1. **Network**: fixed by the binary; it says which.
2. **Role**: *matcher and keepers* (the default, earns, needs a funded key) or *index only* (follows the chain and serves
   the read API; no key, no funds).
3. **Node URL**: default `ws://127.0.0.1:18110` (mainnet) or `ws://127.0.0.1:18210` (testnet-10). It connects at once and
   reports the node's version, network, sync state and UTXO index; on a problem it shows how to start the node and lets you
   check again, enter another URL, or `skip`.
4. **Data directory**: default `kob-data` next to the program (the database and the record log; a few GB).
5. **Operator key** (matcher and keepers): it generates a new key into `kob-operator.key` with the operating system's
   random generator and makes the file readable by your account only (Windows: an ACL with your account alone; Linux: mode
   `0600`), or uses a key file you name (64 hex characters). The secret is never printed. An existing
   `kob-operator.key` is offered again, never replaced.
6. **Funding**: it prints the key's address. Send at least 100 KAS (200 recommended, as a few transfers of 10 KAS or more,
   so several transactions can go out in one tick). It shows the balance as it arrives; press Enter to go on without
   waiting.
7. **x402 facilitator**: off by default (it needs its own configuration, `docs/ops/executor.md`, A.6).

It then writes `kob-executor.toml` next to the program (plus `kob-tokens.json`, the supported token programs, on
testnet-10; the mainnet binary carries its pinned registry), prints the command line the file stands for, and starts the
executor. From then on, a double-click runs that configuration directly.

**Which tokens it lists.** Every order of a token program the binary supports, whatever the token. KOB makes no listing
decision: the registry is a pinned snapshot of facts and of the supported programs, no token is listed by name and none is
refused for what it is (curation belongs to the community token lists a user chooses). `kob-tokens.json` is that program
list for testnet-10 (with no token entries: the registry's entries name mainnet tokens); the mainnet binary lists the same way
from the registry it embeds. A list of your own (`tokens_path` in `kob-executor.toml`) is your policy, not KOB's.

**The key is a hot key.** The executor signs with it unattended. Keep only working capital on it (a few days of fees and
carriers), never savings. Back up `kob-operator.key` somewhere only you can read: whoever has the file has the funds, and
without it they are lost.

## 4. What to watch

Once a minute the executor logs a status line:

```
status: indexer following, caught up (3 DAA behind the node), 120 orders indexed | 14 transactions sent, 12 final,
1 pending, profit of the final ones 0.42 KAS | funds 182.4 KAS | no warnings
```

* On the first start the indexer catches up with everything the node retains (`indexer catching up, ... DAA behind the
  node (about ...)`); the matcher and keepers plan nothing until it is close to the tip.
* `WARNING:` names what needs you: the node not synced or unreachable, funds below 100 KAS (`--low-funds-kas`), the pause
  file present, transactions the node rejected.
* The read API answers on `http://127.0.0.1:8090/v1/health` (loopback only).
* For real monitoring use the metrics file (`--metrics-file`, `docs/ops/executor.md`, Part C *Monitoring*).

## 5. Pause and stop

* **Pause trading** without stopping the indexer: create the file `kob-data/pause` (any content); delete it to resume.
* **Stop**: Ctrl+C in the window (or close it). The database survives; the next start continues where it stopped. A
  start after more than the node's retention (7 days with the flags above) needs the gap runbook
  (`docs/ops/executor.md`, Part B 7.4).
* **Withdraw**: stop the executor, then send the key's KAS elsewhere with `kob-cli` from the same folder:
  `kob-cli send KAS <your address> <amount> --key-file kob-operator.key --network mainnet --node ws://127.0.0.1:18110`
  (`kob-cli send --help`).
* If the start fails, the window stays open with the error until you press Enter.

## 6. Move to a service

The setup printed the exact command line, for example:

```
cd /d C:\kob
C:\kob\kob-executor.exe run --config kob-executor.toml --key-file kob-operator.key --pause-file kob-data/pause
```

Relative paths are relative to the folder of `kob-executor.toml`. To change options, edit `kob-executor.toml` (every key
of `docs/ops/executor.md`, Part B 4; `[launch] args` takes further options of the role, e.g.
`args = ["--sweep-own-strays"]`) or run the setup again (`kob-executor setup`).

**Windows**: a Task Scheduler task "At startup" (run whether the user is logged on or not, as the account that owns the
key file) with program `C:\kob\kob-executor.exe`, arguments `run --config kob-executor.toml --key-file kob-operator.key
--pause-file kob-data/pause` and *Start in* `C:\kob`. Set "If the task fails, restart every 1 minute".

**Linux (systemd)**: copy the folder to `/opt/kob`, then

```
[Unit]
Description=KOB executor
After=network-online.target

[Service]
WorkingDirectory=/opt/kob
ExecStart=/opt/kob/kob-executor run --config kob-executor.toml --pause-file kob-data/pause
LoadCredential=kob-operator-key:/opt/kob/kob-operator.key
User=kob
Restart=always
NoNewPrivileges=yes

[Install]
WantedBy=multi-user.target
```

(`LoadCredential=` hands the key to the process without making the file readable by the service account; the user `kob`
owns `/opt/kob/kob-data`.) The hardened unit of the operations guide is `docs/ops/executor.md`, A.3.

**Docker (Linux)**: the repository's `Dockerfile` builds the executor of one network from the source (pinned base images, a
non-root user, uid 10001). The volume `/data` holds the configuration, the key file and the data directory
(`KOB_EXECUTOR_DIR=/data` in the image: the directory of the setup and of a start without arguments); the setup runs in the
container once, then the container runs what it wrote:

```
docker build --build-arg NETWORK=mainnet -t kob-executor:mainnet .      # or NETWORK=testnet-10
sudo install -d -o 10001 -g 10001 -m 0700 /srv/kob
docker run -it --rm --network host -v /srv/kob:/data kob-executor:mainnet setup --no-start
docker run -d --name kob --restart unless-stopped --network host -v /srv/kob:/data kob-executor:mainnet
docker logs -f kob                                                       # the status lines
```

`--network host` lets the container reach the node's loopback RPC and keeps the read API on the host's loopback. Pause with
`touch /srv/kob/kob-data/pause` (as uid 10001 or root), stop with `docker stop kob`. No image is published: build it from
the commit you verified.

## 7. Upgrade

Stop the executor, replace `kob-executor` (and `kob-cli`) with the new release's files, start it again. The configuration,
the key and the database stay. Read the release notes first: an indexer upgrade can ask for a rebuild
(`docs/ops/release.md`, 7).
