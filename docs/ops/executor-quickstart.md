# KOB executor: quick start for operators

Anyone can run a KOB executor: it follows the chain, lists the order book and, with a small funded hot key, matches
crossing orders and refunds expired ones, earning the spreads and the makers' tips (`docs/ops/executor.md`, Part C,
*Economics*). This page takes you from the download to a running executor. The full operations guide is
`docs/ops/executor.md`.

The executor talks to nothing but the Kaspa node you point it at (public nodes found through the Kaspa resolvers by default,
or your own). It sends no telemetry, charges no fee, does not update itself and connects to no server of the KOB project.

**Risk.** KOB is pre-audit software: its contracts have had no external security review and mainnet launches as a canary
(`SECURITY.md`, `docs/security/AUDIT-2026-10.md`). The executor signs unattended with a **hot key**, so it holds working
capital only (section 3). What one of its transactions can pay in fees is capped by `--fee-max-tx-kas` (default 1 KAS) and
`--fee-max-rate` (default 1,000 sompi per gram, the relay floor being 100; `docs/ops/executor.md`, Part C, *Fee policy*). The
executor signs only inputs of its own key and the covenants enforce every order, but a bad node can still stall it or make it
waste fees, which is why public nodes are used as a cross-checked pair (section 2).

## 1. Download and check

Prebuilt binaries are attached to each [GitHub release](https://github.com/RossKU/kob/releases), one archive per network and
platform:

| Archive | For |
|---|---|
| `kob-executor-<version>-mainnet-x86_64-pc-windows-msvc.zip` | mainnet, Windows 10 / 11 (64-bit) |
| `kob-executor-<version>-mainnet-x86_64-unknown-linux-gnu.tar.gz` | mainnet, Linux x86-64 (glibc) |
| `kob-executor-<version>-mainnet-aarch64-linux-android.tar.gz` | mainnet, an Android phone through Termux (arm64; section 8) |
| `kob-executor-<version>-testnet-10-...` | the same for testnet-10 |
| `kob-executor-<version>-testnet-10-android.apk` | the testnet-10 Android app, a tap-to-use install on an arm64 phone (section 9) |

A binary serves exactly one network (the deployment build, `docs/ops/release.md`): the mainnet binary refuses a testnet
node and the other way round. Start on testnet-10 if you have never run one.

Each archive holds `kob-executor` (the executor), `kob-cli` (the command-line client), this page, `LICENSE` and a
`SHA256SUMS` of its files; the release also lists a `SHA256SUMS` of every archive and of the APK. Check the archive you downloaded against
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
(`cargo build --release --locked -p kob-executor --features deploy-mainnet`) and compare. The archives are neither signed nor
attested, and the `SHA256SUMS` sits on the same release page as the archives, so a matching hash proves that the download is intact, not
that the release is genuine: compare the hashes with the copy in the release announcement, and, for an executor that will hold
funds, rebuild it from the tagged commit.

Unzip it into a folder of its own that your account can write to (for example `C:\kob` or `~/kob`; not `Program Files`):
the setup writes its configuration, the key file and the database next to the program. **Extract the archive first**: do not
double-click the program inside the zip viewer (Windows then runs it from a temporary folder and the key and the database
would be written there), and avoid a folder that a cloud service syncs (OneDrive, Dropbox, ...), which would upload the key file.

**Windows SmartScreen.** The Windows binary is not code-signed, so the first start shows "Windows protected your PC".
Click **More info**, check that the file is the one whose hash you verified, then **Run anyway**. Windows asks once per
file.

## 2. A Kaspa node: public nodes, or your own

**Public nodes (the default, nothing to install).** The setup's first choice: the executor finds public Kaspa nodes
through the Kaspa resolvers (the same ones the Kaspa wallets use) and connects to them over TLS. It always uses **two**, on
different hosts: one is the chain authority, the other cross-checks it (only the chain blocks and transactions both report
are indexed), and every transaction goes to both. If one fails, the other carries on at once and a replacement on a third
host is found in the background. Public nodes are shared and slower than your own, and the first catch-up takes longer. A
bad node cannot steal funds (the executor signs only its own inputs, and the covenants enforce every order), but it could
waste fees or hide orders: the cross-check is the guard. Good to start with and for a small operator.

**Your own node (recommended for serious operators, required for Miner Mode).** Faster, not shared, and nobody between
you and the chain. A rusty-kaspa node (v2.1.0 or later) of the same network on the same machine, with its JSON wRPC
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
2. **Node**: public Kaspa nodes (the default: it finds the two, checks each one's network, sync state and UTXO index and
   shows their host names) or your own node (section 2).
3. **Role**: *matcher and keepers* (the default, earns, needs a funded key) or *index only* (follows the chain and serves
   the read API; no key, no funds). For your own node it then asks its URL: default `ws://127.0.0.1:18110` (mainnet) or
   `ws://127.0.0.1:18210` (testnet-10), connects at once and reports the node's version, network, sync state and UTXO index;
   on a problem it shows how to start the node and lets you check again, enter another URL, or `skip`.
4. **Data directory**: default `kob-data` next to the program. The prompt reads "the database and the record log; a few GB",
   which fits the index-only role. For *matcher and keepers* the setup also turns on the fast start and **lean storage**: no
   record log and a database capped at 500 MB (section 8, point 5; `docs/ops/executor.md`, A.1.2).
5. **Operator key** (matcher and keepers): it generates a new key into `kob-operator.key` with the operating system's
   random generator and makes the file readable by your account only (Windows: an ACL with your account alone; Linux: mode
   `0600`), or uses a key file you name (64 hex characters). The secret is never printed. An existing
   `kob-operator.key` is offered again, never replaced.
6. **Funding**: it prints the key's address. Send at least 100 KAS (200 recommended, as a few transfers of 10 KAS or more,
   so several transactions can go out in one tick). It shows the balance as it arrives; press Enter to go on without
   waiting. The address is not lost if you do: every start prints it (`Funding address ...` under the command line, and
   `funding_address` in the first log line), the low-funds warning names it, and `kob-executor address-balance` prints it
   with the balance (section 5).
7. **x402 facilitator**: off by default (it needs its own configuration, `docs/ops/executor.md`, A.6).

It then writes `kob-executor.toml` next to the program (plus `kob-tokens.json`, the supported token programs, on
testnet-10; the mainnet binary carries its pinned registry), prints the command line the file stands for, and asks
`Start the executor now? [Y/n]`: Enter starts it, `n` leaves the files and ends the setup (`setup --no-start` does not ask).
From then on, a double-click runs that configuration directly. If another kob-executor already runs from the same folder,
the start is refused at once with a message naming it, before anything else is printed or logged.

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

* With public nodes the line ends with both hosts and their state, e.g. `nodes: muon-10.kaspa.blue (authority, ok),
  vector-10.kaspa.green (cross-check, ok, replaced 1x)`: `replaced` counts nodes given up (failed, or disagreeing), and
  `disagreements` answers of the authority the other node contradicted (nothing contradicted is indexed). The two public
  nodes' newest blocks often differ for a moment; that settles by the next poll, is not a disagreement, and is only counted
  (`tip_differs` in `/v1/health`) and logged at debug level. A warning appears if it does not settle.
* The first start trades within seconds: the setup turns on the fast start, so the executor follows the chain from the node's
  current tip at once (`fast start: trading on orders seen since 14:32 UTC; full book in about 7 h 40 min`) while the
  history is indexed in the background; it misses the orders created before it started until the full book is there
  (`/v1/health`: `book_complete: false`), then switches over by itself (`docs/ops/executor.md`, A.1.1). Without it (an older
  configuration, `fast_start = false`) the indexer first catches up with everything the node retains and the matcher and
  keepers plan nothing until it is close to the tip. The banner at the start says which of the two applies.
* `WARNING:` names what needs you: the node not synced or unreachable, funds below 100 KAS (`--low-funds-kas`; the warning
  names the address to send KAS to; it is logged at the start, then at most every 10 minutes or when the balance changes by a
  quarter, and the status line shows the funds every minute; the executor trades with less than 100 KAS, the number is an alert
  threshold), the pause file present, transactions the node rejected.
* The read API answers on `http://127.0.0.1:8090/v1/health` (loopback only).
* For real monitoring use the metrics file (`--metrics-file`, `docs/ops/executor.md`, Part C *Monitoring*).
* **Keep the computer's clock right** (automatic time sync on). The matcher compares a day order's UTC deadline with the system
  clock (a fast clock skips orders that are still valid, a slow one can fill past a deadline), and TLS to public nodes fails
  with a wrong clock. The executor does not check the clock.
* **Port 8090 busy.** The read API listens on `127.0.0.1:8090` by default. If another program (or a second KOB executor for
  another network) holds the port, the executor still runs but logs `API server stopped: ...` and serves no API, although the
  start banner printed the address. Change `[api] listen` in `kob-executor.toml` (or `--listen`) and restart.

**What it costs to run.** Measured and estimated figures for sizing a machine or a metered line (`docs/ops/executor.md`, Part B 3 and A.1.2):

| | Matcher and keepers (lean, fast start) | Index only (full history, record log) |
|---|---|---|
| Disk | database capped at 500 MB (`lean_max_db_mb`), no record log, plus a constant 42 MB window of chain blocks | grows with the orders: plan on about 10 KB per order in all (soak measurements: 1.6 GB of database for 295,000 orders and 2.85 GB for 501,000, i.e. 5.4 to 5.7 KB each, plus a record log of about 3.9 KB per order, 1.2 GB for 306,000); a quiet testnet-10 is a few GB |
| Memory | 0.3 to 1 GB typical, up to 4 GB while catching up on mainnet-sized batches | the same |
| CPU | one core at most; 2 to 4 % of a core at 3,000 transactions per second of chain load | the same |
| Network | about 1.4 KB per chain transaction from the node (1.35 MB/s at 1,000 transactions per second); the first catch-up over public nodes is a few GB (about 4 GB on a phone), then the steady flow; quiet testnet-10 measured 15.7 GB per day on 2026-09-29 | the same |

Mainnet record-log growth under real traffic is not measured yet. The status line's profit counts the final transactions since this
run started; it is kept in memory only and starts again at zero after a restart, so the executor keeps no earnings history; to
keep one, record the metric `kob_matcher_finalized_profit_sompi` (`--metrics-file`) with your monitoring.

## 5. Pause and stop

* **Pause trading** without stopping the indexer: create the file `kob-data/pause` (any content); delete it to resume.
* **Stop**: Ctrl+C in the window (or close it; `SIGTERM` / `docker stop` / `systemctl stop` do the same). The executor
  stops within a few seconds even when the node does not answer, flushes the database and exits; press Ctrl+C a second
  time to exit at once. The database survives either way; the next start continues where it stopped. A start after more
  than the node's retention (7 days with the flags above) continues from the node's pruning point on its own and says so
  (`gaps` in `/v1/health`, the log; `docs/ops/executor.md`, Part B 7.4).
* **Address and balance**: `kob-executor address-balance` (in the folder of `kob-executor.toml`, or with `--dir <folder>`)
  prints one JSON object: the key's address, the network and its plain balance read from the configured node
  (`balance_sompi`, `balance_kas`; when the node cannot be read, `balance_sompi` is `null` and `error` says why). It never
  prints the secret and can run while the executor runs. Its sibling for scripts and the Android app is `kob-executor setup
  --non-interactive --node resolver|<ws-url> --role match|index [--new-key] [--network mainnet|testnet-10] [--dir <folder>]`:
  no questions, no start; it writes the same files as the dialogue (replacing the configuration, keeping an existing key
  file), and prints one JSON object with the `address` (`null` for index only) and the paths. `--network` is only for a
  development build; a deployment build knows its own and refuses another.
* **Withdraw**: stop the executor first, then, from the same folder (or with `--dir <folder>`, or `--config <file>` for a
  configuration elsewhere):

  ```
  kob-executor withdraw --to <address> --amount 25.5        # or --amount all
  ```

  It uses the executor's own node connection from `kob-executor.toml`, so it works with the default public nodes (wss,
  through the resolvers) as well as with your own node, and needs no other tool. It lists the key's plain KAS coins at that
  node and builds one payment from the largest coins first (at most 50 inputs). `--amount all` sends everything those coins
  hold minus the fee; if more coins are left it says so, and you run it again once the first is accepted. It shows the
  network, both addresses, the amount and the fee, and asks you to type `yes`; `--yes` skips the question (scripts), and
  without `--yes` and without a terminal it refuses to send. `--dry-run` shows the payment and sends nothing; `--fee-rate`
  (default 100, the relay floor) pays more per gram of mass. The address must belong to the binary's network (a mainnet
  binary refuses a `kaspatest:` address, and the node must report the same network). **A running executor makes it
  refuse**: the command takes the data directory's writer lock, the one `run` holds ("in use by another kob-executor
  writer: stop it first"), so no matcher transaction is being built from the coins it spends and two withdrawals cannot
  overlap. A transaction the executor sent just before it stopped may still be on its way: if the node says a coin is
  already spent, nothing was lost, wait a minute and run it again. Only plain KAS moves; tokens the key holds stay where
  they are. Afterwards `kob-executor address-balance` shows what is left, and the next start warns "funds low" until you fund
  the key again. (`kob-cli send` also works, but only against your own node on `ws://`: it has no resolver or `wss`
  support, and its amount needs a unit such as `2.5KAS`; see `crates/kob-cli/README.md`.)
* If the start fails, the window stays open with the error until you press Enter.

**Back up, and move to another computer.** The only thing you cannot rebuild is the key: back up `kob-operator.key` (a plain
64-hex-character private key; there is no seed phrase) where only you can read it. The configuration is a text file
(`kob-executor.toml`) that setup writes again in a minute. The database is not precious: a matcher on lean storage rebuilds it
from the node within seconds on a start with an empty data directory (fast start). To move an executor:

1. **Stop the old one first** (section 7 shows how). Two executors on one key spend each other's coins and waste fees; the writer
   lock protects one data directory, not a key.
2. Copy the program folder without `kob-data` (or run the setup on the new machine and name the copied key file when it asks).
   The paths in `kob-executor.toml` are relative to the file; check `rpc_url` (an own node's address differs per machine).
3. On Linux the key file must be readable by its owner only (`chmod 600 kob-operator.key`): the executor refuses a key file that
   group or others can read. On Windows a copy takes the permissions of the folder it lands in: check that only your account can read the
   key (the setup made it so).
4. Start the new one and look at `kob-executor address-balance` and the status line before you delete the old folder.

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
--pause-file kob-data/pause` and *Start in* `C:\kob`. Set "If the task fails, restart every 1 minute". A task has no
console: its log is the file `kob-executor.log` the setup configured (see *Logs* below); read it with
`Get-Content C:\kob\kob-executor.log -Tail 50 -Wait`. In a window you opened yourself, do not click inside the console
while the executor runs: a mouse selection in the classic Windows console (Quick Edit mode) pauses the program's output
and with it the executor until you press Enter or Esc. That freeze is standard Windows behaviour that we have not been able
to reproduce on the executor, so treat it as a precaution (console window Properties, Options, untick *QuickEdit Mode*, if
you keep a window open); a scheduled task has no console to click in.

**Logs.** The log goes to the console. The interactive setup also writes `log_file = "kob-executor.log"` into
`kob-executor.toml` (a path relative to that file; not for the index-only role, and not for the Android app's
non-interactive setup), and any start can set one with `--log-file <path>` (or `KOB_LOG_FILE`) or `log_file` in the
configuration. With a log file the console still shows the log while it is a terminal (a double click) and shows nothing
when it is not (a task, a service, a redirect): the file is then the log. The file is rotated by size: at `log_max_mb`
(`--log-max-mb`, default 10) it becomes `<file>.1`, the older ones move up to `.2`, ..., and the one past `log_files`
(`--log-files`, default 5; 0: start over) is deleted, so the log never takes more than (`log_files` + 1) x `log_max_mb` MB.
ANSI colour codes are written only to a console that is a terminal (and not when `NO_COLOR` is set); the file never has
them. `RUST_LOG` (default `info`) sets the detail. Under systemd the journal already keeps the log (`journalctl -u kob -f`).

**Linux (systemd)**: copy the folder to `/opt/kob`, then

```
[Unit]
Description=KOB executor
After=network-online.target
StartLimitIntervalSec=0

[Service]
WorkingDirectory=/opt/kob
ExecStart=/opt/kob/kob-executor run --config kob-executor.toml --pause-file kob-data/pause
LoadCredential=kob-operator-key:/opt/kob/kob-operator.key
User=kob
Restart=always
RestartSec=10
NoNewPrivileges=yes

[Install]
WantedBy=multi-user.target
```

(`LoadCredential=` hands the key to the process without making the file readable by the service account; the user `kob`
owns `/opt/kob/kob-data`. `RestartSec=10` and `StartLimitIntervalSec=0` make systemd keep retrying: without them it gives up after
five starts in ten seconds, for example while the network is still coming up after a boot.) The hardened unit of the operations guide is `docs/ops/executor.md`, A.3.

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

## 7. Upgrade, stop and withdraw

**Upgrade an executor** (matcher and keepers, or index only). The configuration, the key and the data directory stay; only the
program changes. The program is not replaced while it runs, and it does not update itself.

1. **Read the release notes.** They say whether the release changes the `KOB1` payload version (indexers must go first,
   `docs/ops/release.md`, 7) or the database (brought up to date in place, or rebuilt: step 6).
2. **Note what runs now:** `kob-executor --version` (the crate version, the release tag without the `v`) and
   `curl -s http://127.0.0.1:8090/v1/health` (`version`, `commit`, `payload_version`; the API is loopback only unless you
   changed it). A build made from `master` before the release bump says `0.0.0`; tell such builds apart by `commit`.
3. **Download and check** the new archive (section 1). Unzip it into a new folder; the archive holds the programs and this page,
   never your configuration.
4. **Stop gracefully** and swap:
   * a window: Ctrl+C (or close the window); Linux service: `systemctl stop kob`; Docker: `docker stop kob`; Windows Task
     Scheduler: Ctrl+C in a window you opened yourself is the clean way; *End* on a task kills the process, which the database
     survives (below). The executor stops within seconds, flushes the database and exits; a second Ctrl+C exits at
     once, and a stop that has not finished in ten seconds exits by itself (exit code 2, logged). The database survives either
     way (SQLite WAL, one transaction per batch), so a forced kill costs a short recovery, not data.
   * keep the old program (`kob-executor.old`) and put the new `kob-executor` (and `kob-cli`) in its place; on Windows the old file
     is locked until the process has ended.
   * if you may want to roll back across a database change, copy `kob-data` aside now while the executor is stopped (a newer
     database schema is refused by an older program).
5. **Start** it (double-click, `systemctl start kob`, `docker run` with the same volume and the rebuilt image). It continues from
   the stored cursor. Check `kob-executor --version`, then `/v1/health`: `version`, `commit` and `payload_version` must be the
   release's, `state` becomes `following`, and the status line shows no `WARNING:`.
6. **When the release notes ask for a rebuild** (the database is refused as old): a matcher on lean storage has no record log to
   replay (`docs/ops/executor.md`, 7.6), but it does not need one: stop it, move `kob-data` aside (or delete it) and start;
   the fast start rebuilds the live book within seconds. Make sure the key file is not inside `kob-data` (setup puts it next
   to the program), and create `kob-data/pause` again if you use it. An index-only archive rebuilds with `kob-executor index
   replay` while offline; the public read server has a way without downtime (`docs/ops/chart-server.md`, section 6).
7. **Rollback**: stop, put `kob-executor.old` back, restore the `kob-data` copy of step 4 if the database changed. An indexer must
   not go below the payload version in use (`docs/ops/release.md`, 7).

The Android app updates by installing the newer APK over the old one when both have the same signature (section 9, *Updates*); a
Termux executor follows the steps above in its folder; for Docker rebuild the image from the commit you verified.

**Version check on someone else's indexer:** `curl -s https://<indexer>/v1/health` shows `version`, `commit` and
`payload_version` (also `GET /v1/health` in `docs/api.md`).

**Stop for good, or empty the key.** Stop the executor (above), then withdraw (section 5, *Withdraw*:
`kob-executor withdraw --to <address> --amount all`). It refuses while an executor runs from the same data directory, and moves
only plain KAS: tokens the key holds stay where they are. Check with `kob-executor address-balance`.

**Logs and rotation.** The flags are `--log-file <path>` (or `KOB_LOG_FILE`), `--log-max-mb` (default 10; 0: never rotate) and
`--log-files` (rotated files kept, default 5; 0: the file starts over when full), or the keys `log_file`, `log_max_mb`,
`log_files` in `kob-executor.toml` (section 6, *Logs*). They are global: they work on every subcommand.

## 8. Run on an Android phone (Termux)

An arm64 Android phone (Android 7.0 or later) can run the executor in [Termux](https://termux.dev), a terminal app. This is the terminal route (a tap-to-use testnet-10 app is section 9).
You use the `...-aarch64-linux-android.tar.gz` archive of section 1, which is built for Android's own C
library, so name resolution and TLS to the public nodes work as in any Termux program.

1. **Install Termux from F-Droid** (or the Termux GitHub releases), **not from Google Play**: the Play Store build is
   not maintained like the F-Droid one and its packages do not match. Open it and run `pkg update`.
2. **Tools**: `pkg install curl tar`.
3. **Download and check** in the Termux home folder (the data must stay in Termux's private storage, not in `/sdcard`,
   which has no file modes). Take the archive of your network from the release page:

   ```
   cd ~
   curl -LO https://github.com/RossKU/kob/releases/download/v<version>/kob-executor-<version>-mainnet-aarch64-linux-android.tar.gz
   curl -LO https://github.com/RossKU/kob/releases/download/v<version>/SHA256SUMS
   sha256sum -c --ignore-missing SHA256SUMS      # must print "...: OK"
   tar xzf kob-executor-<version>-mainnet-aarch64-linux-android.tar.gz
   cd kob-executor-<version>-mainnet-aarch64-linux-android
   chmod +x kob-executor kob-cli
   ./kob-executor
   ```

   The setup of section 3 follows. Choose **public nodes** (the default: a phone should not run a node), keep the default
   data directory `kob-data` and the key file next to the program, i.e. inside the Termux home. The key file gets mode
   `0600`. Back it up where only you can read it.
4. **Keep it running.** Android freezes background apps, and a stopped Termux is a stopped executor:
   * Pull down the Termux notification and tap **Acquire wakelock** (or run `termux-wake-lock` if the Termux:API add-on is
     installed; the notification button needs nothing extra). Keep the phone on a charger.
   * Settings, Apps, Termux, Battery: set it to **Unrestricted** (disable battery optimisation). Some makers add their own
     "app launch" or "sleeping apps" lists (Samsung, Xiaomi, Huawei, OnePlus): exempt Termux there too.
   * Keep Termux in the recent apps (do not swipe it away) and the screen can be off. After a restart of the phone, start
     it again by hand: `cd ~/kob-executor-<version>-mainnet-aarch64-linux-android && ./kob-executor`.
5. **Data and storage.** With public nodes the history download is about 4 GB (use Wi-Fi; the executor trades meanwhile), then the steady flow is far
   smaller. The setup turns on lean storage (`lean = true`): the database keeps the live book and 30 days of closed orders (and never more than
   500 MB), with no record log; the history comes from an archive indexer.

**Warning.** A phone is easy to lose and Android may stop the executor at any time (memory pressure, a battery saver, an
update, a restart). The key on it is a hot key: put **only a small working capital** on it (a few days of fees and
carriers, section 3), never savings, and expect to restart it. A stopped executor loses nothing but the spreads it would
have earned; its database survives and it continues where it stopped (section 5). If you would rather not risk any funds on
the phone, choose the **index only** role in the setup: it follows the chain and serves the read API on the phone with no
key and no funds.

## 9. Android app (testnet-10, sideloaded)

A tap-to-use app for the same arm64 phones (Android 7.0 or later): the testnet-10 executor in a foreground service, with a
setup screen, a start / stop switch, a status screen, **Withdraw funds** and **Export the key**. It runs the `kob-executor`
binary of section 8 inside the app (`android/`), keeps its configuration, key and database in the app's private storage, and
talks to nothing but the Kaspa node(s) you choose: no telemetry, no fee, no project server. It is a **testnet-10 build only**
for now. It is sideloaded (no store).

1. **Download the APK.** On the [GitHub release](https://github.com/RossKU/kob/releases) of the version, download
   `kob-executor-<version>-testnet-10-android.apk` (on the phone, or on a computer and copy it over) and check its SHA-256
   against the release's `SHA256SUMS`. Builds of the latest `master` are also the artifact of the GitHub Actions workflow
   **android** (a zip holding the APK; needs a GitHub login, kept for a limited time; the run's summary prints the SHA-256).
   A file name ending in `-debug-signed.apk` is signed with a throwaway key: read **Updates** below before you put funds on it.
2. **Install it.** Open the APK from the phone's file manager or browser and allow **install from this source** when Android
   asks (Settings, Apps, Special access, Install unknown apps). Android may warn about an app from an unknown source: it is
   not from a store.
3. **Set up.** Open *KOB Executor*. Keep **Public nodes** (two nodes found through the Kaspa resolvers; the default of
   section 8) or enter your own node's URL, choose **Matcher and keepers** or **Index only**, and tap *Save and generate
   key*. For the matcher the app shows its **funding address** (always on the main screen, with a *Copy address* button):
   send it at least 100 testnet KAS (200 recommended; they have no value, mine or ask a faucet), and the balance appears
   under it. The key is generated on the phone (`kob-operator.key`, readable by the app only) and never leaves it unless you
   export it. **Uninstalling the app or clearing its data (Settings, Apps, KOB Executor, Storage, Clear data) deletes the key,
   and the funds on it with it.**
4. **Allow background running.** Tap *Battery: allow unrestricted background use* and accept (or Settings, Apps, KOB
   Executor, Battery, **Unrestricted**), and exempt the app from the maker's own "sleeping apps" lists (Samsung, Xiaomi,
   Huawei, OnePlus). Allow notifications: the executor shows a persistent notification while it runs. The screen can be off.
5. **Start.** Use **Wi-Fi and a charger** for the first start: with public nodes the history download is about 4 GB and takes
   hours. The executor trades within a minute on the orders seen since it started (the status screen says "Order book:
   partial" with the time until the full book is there) and fills in the history meanwhile. Then switch **Run the executor** on. If the executor exits, the app starts it
   again (after 5 s, then longer); switch it off to stop it. The status screen shows whether it is caught up, its lag, the
   two nodes in use, the transactions it sent and its funds, and warnings (paused, funds low, node trouble).

**Withdraw funds.** The button under the funding address opens a screen with the address to send to, an amount in KAS or
`all`, and a confirmation. It **stops the executor first** (the executor uses the funds, and the database allows one writer),
runs `kob-executor withdraw --to <address> --amount <KAS|all> --yes` (section 5) over the node of your setup, shows the
transaction id or the error, and offers to **start the executor again**. A sent transaction cannot be taken back: check the
address.

**Export the key** (backup). The button opens a screen that warns first, asks for a confirmation, and then shows the key (64
hex characters, selectable) with *Copy the key* and *Save the key to a file* (you choose where, with the phone's file picker).
Anyone who has the key can take the funds: keep a copy where only you can read it, never in a photo or a message. The screen
hides the key again when you leave it, and Android cannot screenshot it. Export the key before you uninstall the app or clear
its data. The key is a plain private key; the app itself has no import.

**Updates.** Android installs an APK over an installed one only when **both are signed with the same key**; then the app's data
(the key, the configuration, the database) stays. APKs signed with the release key (the file name has no `-debug-signed`; the
signer certificate's SHA-256 is in the workflow run's summary) update each other in place, and `versionCode` grows with every
build, so any newer release installs over an older one. **A `-debug-signed` APK cannot be updated**: every build has another
throwaway key, Android answers "App not installed", and the only way to a newer build is uninstalling the old one, which deletes
its key and funds. With such an APK, put only test coins on it, and use *Withdraw funds* before you replace it. Android refuses
a downgrade; `versionName` is the workspace version of the release.

**Small funds only.** It is a hot key on a phone that Android may stop at any time. Put only a few days of fees and carriers
on it, never savings; a stopped executor loses nothing but the spreads it would have earned.

**Release signing (for the maintainer).** The release key is created once, outside the repository, and stored as four GitHub
repository secrets that `.github/workflows/android.yml` (the push builds and, through `release.yml`, the release build) signs
with:

1. Run `node scripts/make-android-keystore.mjs` (needs a JDK's `keytool`). The default folder is `~/.kob/android-signing`
   (`C:\Users\<you>\.kob\android-signing` on Windows); `--out <folder>` changes it, and a folder inside the repository is refused.
   It writes the keystore, a random password file and the keystore as base64, **prints no password**, prints the signer
   certificate's SHA-256, and prints the exact steps of the next point.
2. Store the four secrets, with the GitHub CLI:

   ```
   # bash
   gh secret set ANDROID_KEYSTORE_B64      --repo RossKU/kob < ~/.kob/android-signing/kob-release.jks.b64.txt
   gh secret set ANDROID_KEYSTORE_PASSWORD --repo RossKU/kob < ~/.kob/android-signing/kob-release.password.txt
   gh secret set ANDROID_KEY_PASSWORD      --repo RossKU/kob < ~/.kob/android-signing/kob-release.password.txt
   gh secret set ANDROID_KEY_ALIAS         --repo RossKU/kob --body kob-release
   # PowerShell
   Get-Content -Raw $HOME\.kob\android-signing\kob-release.jks.b64.txt  | gh secret set ANDROID_KEYSTORE_B64      --repo RossKU/kob
   Get-Content -Raw $HOME\.kob\android-signing\kob-release.password.txt | gh secret set ANDROID_KEYSTORE_PASSWORD --repo RossKU/kob
   Get-Content -Raw $HOME\.kob\android-signing\kob-release.password.txt | gh secret set ANDROID_KEY_PASSWORD      --repo RossKU/kob
   gh secret set ANDROID_KEY_ALIAS --repo RossKU/kob --body kob-release
   ```

   or in the browser: the repository, Settings, Secrets and variables, Actions, *New repository secret*, once for each of
   `ANDROID_KEYSTORE_B64` (the content of `kob-release.jks.b64.txt`), `ANDROID_KEYSTORE_PASSWORD` and `ANDROID_KEY_PASSWORD`
   (both the content of `kob-release.password.txt`) and `ANDROID_KEY_ALIAS` (`kob-release`). `node scripts/make-android-keystore.mjs --steps`
   prints the steps again for the files that exist.
3. Run the **android** workflow (Actions, android, *Run workflow*): it must not warn "Android APK is debug-signed", and the
   signer certificate SHA-256 in its summary must be the one the script printed.
4. **Back up the folder** (a password manager, an offline copy). If the keystore is lost, installed apps can never be updated
   in place again. Never commit it (`*.jks`, `*.keystore`, `*.p12` are git-ignored) and never paste the password in a chat or issue.

Without the four secrets the workflows still build: they sign with the debug key, name the file `...-debug-signed.apk`, put a
warning on the run and, for a release, in the draft release's notes. Either way the APK is built `debuggable=false`, so
`adb run-as` cannot read the key off a phone.

`versionCode` is the commit time in minutes (`git log -1 --format=%ct` divided by 60, set by the workflows), so it is
deterministic for a commit and grows along `master`; a tag's release is an update of every earlier build. A local Gradle build
without it gets `versionCode` 1 (set `KOB_VERSION_CODE` to build an update).
