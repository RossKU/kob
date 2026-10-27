# A public read server for the web app (`kob-executor index` behind Cloudflare Tunnel)

The hosted web app (`docs/ops/web-hosting.md`) reads the order books, charts, trades and orders from an indexer: the read API of
`kob-executor index` (`docs/ops/executor.md`, Part B). This page runs one for the public on a home PC (or a VPS) and publishes
it through Cloudflare Tunnel (free), without opening a port on the router.

**What this server is not.** It runs the `index` role only: it holds **no key**, has **no funds**, does **no matching**, keeps
no keeper and no x402 facilitator. It follows the chain and answers read-only HTTP and WebSocket requests. It cannot place,
fill, cancel or refund anything, and it cannot forge what the app signs (the app checks every order against the chain
programs before it builds a transaction; `web/README.md`). Executors that match orders are run by third parties.

**It is one indexer among others.** The app must work against any operator's indexer. List independent third-party indexers in
the deployment's `KOB_PAGES_EXTRA_INDEXER_URLS` when they exist: the app cross-checks the key facts of a token (powers, price
scale, best prices) against them and refuses a market order whose start disagrees (`web/README.md`, *Several indexers*). Users
can switch the indexer in Settings. This server is a convenience, not an authority.

**Start here: the order of the first setup.** The web app's deployment needs this server's public address, and this server's
address needs a domain, so do it in this order:

1. Get a **domain whose DNS is on Cloudflare** (a domain you buy, or one you already own and move to Cloudflare; free plan). A
   tunnel hostname cannot be a `pages.dev` name. Decide the hostname now, for example `chart.example.org`; it need not resolve
   yet, so the name can already go into the Pages variable before the tunnel is live.
2. Section 1: the indexer on this machine (stop it before you edit its `[api]`).
3. Section 2: the tunnel and the DNS record; check `https://chart.example.org/v1/health` from another network.
4. `docs/ops/web-hosting.md`: the Pages project, the API token and the repository secrets and variables, with
   `KOB_PAGES_INDEXER_URL=https://chart.example.org`. The workflow does nothing until that variable is set. After you change a
   repository variable later, run *Actions* > *deploy-pages* > *Run workflow* on `master`: a changed variable alone deploys nothing.
5. Section 5: set up an alert before you rely on it.

## 1. The indexer

Download and check the binary of the network (testnet-10 for now): `docs/ops/executor-quickstart.md`, section 1.

**Node.** Either your own rusty-kaspa node on the same machine (recommended for a public server: faster, and nobody between
you and the chain; `docs/ops/executor-quickstart.md`, section 2), or the public nodes the Kaspa resolvers name
(`rpc_url = "resolver"`: always a cross-checked pair on two hosts; `docs/ops/executor.md`, Part C, *Node*).

**Setup.** Run `kob-executor setup` in the folder of the binary and answer: the node (public nodes, or your own), **role:
index only**, the data directory (`kob-data`). There is no key question for this role. It writes `kob-executor.toml` (and on
testnet-10 `kob-tokens.json`, the supported token programs with no token entries: every token of a supported program is indexed
and listed by the API, and none is named; the web app's market list shows the tokens that have a ticker or declared decimals and keeps the
others behind "Show N token(s) without a name"). **The setup starts the executor when it finishes, with the default `[api]`** (loopback
`127.0.0.1:8090`, no trusted proxy, no CORS): stop it (close its window, or Ctrl+C) before you edit the file, and start it again afterwards,
or the settings below are not read. Then add the read API settings for the tunnel at the end of `kob-executor.toml`:

```toml
[api]
listen = "127.0.0.1:8090"                  # loopback only: the tunnel is the one way in
trusted_proxies = ["127.0.0.1", "::1"]     # cloudflared connects from this machine
client_ip_header = "cf-connecting-ip"      # the visitor's address, set by Cloudflare's edge
cors_allow_origin = "*"                    # the app is served from another origin (see section 3)
metrics = false                            # do not publish GET /v1/metrics through the tunnel (see section 2)
```

A complete file looks like this (`network`, `rpc_url`, `data_dir`, `tokens_path` and `[launch]` as the setup wrote them):

```toml
network = "testnet-10"
rpc_url = "resolver"                       # or "ws://127.0.0.1:18210" for your own testnet-10 node
data_dir = "kob-data"
tokens_path = "kob-tokens.json"            # testnet-10 only; the mainnet build uses the registry it embeds

[launch]
role = "index"

[api]
listen = "127.0.0.1:8090"
trusted_proxies = ["127.0.0.1", "::1"]
client_ip_header = "cf-connecting-ip"
cors_allow_origin = "*"
metrics = false
```

Start it with a double-click on `kob-executor` (or `kob-executor index --config kob-executor.toml`). Running the setup again
replaces the file: add the `[api]` table again afterwards. Run it as a service as `docs/ops/executor-quickstart.md`,
section 6 shows (the index role has no key file and no pause file: arguments `index --config kob-executor.toml`).

**First start: the history and the fast route.** The first start reads everything the node retains, from its pruning point. The API
answers from the start and the app shows the indexer as catching up until it is close to the tip. Over the public nodes of the
resolver that took about 2.4 hours for 36 hours of history (about 14.6 s of chain per second), and the store grows by about
54 MB per 1.4 hours of history (about 1.4 GB in all). The status line and `/v1/health` say how far it is:
"`indexer catching up, ... behind the node (about 35 h), indexing 15x chain speed, caught up in about 2 h 24 min`" (`catch_up` in
`/v1/health`: `rate_daa_per_sec`, `eta_seconds`).

`fast_start` is for the matcher (`kob-executor run`, which trades on a second store while it indexes the history); the `index` role
refuses it (`fast_start = true` or `--fast-start` stop the start with a message). The fast route of a read server is
`start = "sink"` (top level of `kob-executor.toml`, or `--start sink`; on a data directory that is still empty): it follows the chain
within a second of the start and lists the orders created since. The orders older than the start are **not** in that store, for
good: `/v1/health` reports `book_complete: false` and `history_from_daa` (where its history begins) and the status line says so, so
a chart server that starts at the sink has only the books and trades from its start on. A public server that should show the whole
history lets the first start catch up (or restores a backup of a full store, section 4) and does not use `start = "sink"`.

## 2. Cloudflare Tunnel

The tunnel is an outbound connection from this PC to Cloudflare; Cloudflare serves `https://chart.example.org` and passes each
request through it to `http://127.0.0.1:8090`. TLS, the certificate and DDoS filtering are Cloudflare's; the PC needs no open
port and no fixed address. It needs a domain whose DNS is on Cloudflare (free plan).

1. Install `cloudflared` (https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/):
   Windows `winget install --id Cloudflare.cloudflared`, Debian / Ubuntu the `.deb` of Cloudflare's package repository.
2. Log in once (opens the browser; choose the domain): `cloudflared tunnel login`.
3. Create the tunnel and its DNS record:

   ```
   cloudflared tunnel create kob-chart
   cloudflared tunnel route dns kob-chart chart.example.org
   ```

   `create` prints the tunnel's id and writes its credentials file (`~/.cloudflared/<id>.json`, Windows
   `%USERPROFILE%\.cloudflared\<id>.json`). Keep that file private: it lets anyone run the tunnel.
4. Write `config.yml` next to it (`~/.cloudflared/config.yml`, or any path given with `--config`):

   ```yaml
   tunnel: <tunnel id>
   credentials-file: C:\Users\<you>\.cloudflared\<tunnel id>.json   # Linux: /home/<you>/.cloudflared/<tunnel id>.json
   ingress:
     # the read API only: REST under /v1/ and the WebSocket /v1/ws (cloudflared passes the upgrade through)
     - hostname: chart.example.org
       path: ^/v1/
       service: http://127.0.0.1:8090
     - service: http_status:404
   ```

   Use `127.0.0.1`, not `localhost` (the API listens on IPv4 loopback only). The app derives the WebSocket from the indexer URL:
   `https://chart.example.org` -> `wss://chart.example.org/v1/ws`; the `^/v1/` rule covers it.
5. Run it: `cloudflared tunnel --config <path to config.yml> run kob-chart`. As a service: on Linux
   `sudo cloudflared --config /etc/cloudflared/config.yml service install` (copy the config and the credentials file to
   `/etc/cloudflared/` first); on Windows a Task Scheduler task "At startup" with program `cloudflared.exe` and arguments
   `tunnel --config C:\kob\cloudflared.yml run kob-chart`, restarted on failure, like the executor's task.
6. Check from another network: `curl -s https://chart.example.org/v1/health` answers the indexer's status, and
   `curl -sI https://chart.example.org/` answers 404 (nothing but `/v1/` is reachable).

`/v1/metrics` is under `/v1/` and so is public through this rule, as are the node host names in `/v1/health` (`nodes[].node`).
The metrics are the follower's counters in the Prometheus text format; nothing in the app reads them. Do not publish them:
set `metrics = false` in `[api]` (section 1: the route is then not served, 404), and/or block the path in front of the API, in the
tunnel's ingress before the `^/v1/` rule (the first rule that matches wins):

```yaml
     - hostname: chart.example.org
       path: ^/v1/metrics$
       service: http_status:404
```

(or a Cloudflare WAF custom rule that blocks the URI path `/v1/metrics`). An operator who scrapes them uses `--metrics-file`
(`docs/ops/executor.md`, Part C, *Monitoring*) or the loopback listener, not the tunnel.

Set `KOB_PAGES_INDEXER_URL` to `https://chart.example.org` (`docs/ops/web-hosting.md`, section 3).

(The tunnel can also be created in the dashboard, *Zero Trust* > *Networks* > *Tunnels*, with a public hostname
`chart.example.org`, path `^/v1/`, service `HTTP 127.0.0.1:8090`; `cloudflared service install <token>` then runs it.)

## 3. Client addresses, CORS and caching behind the tunnel

**Client addresses.** Every request arrives from `cloudflared` on this machine, i.e. from 127.0.0.1. Without
`trusted_proxies` all visitors would share one client's rate limits and connection caps (20 requests per second, 20
WebSockets, ...). The API believes a forwarding header only from a peer in `api.trusted_proxies` (`crates/kob-executor/src/api/client_ip.rs`):

* `trusted_proxies = ["127.0.0.1", "::1"]`: cloudflared's address; requests from any other peer are keyed by their own address
  and their headers ignored.
* `client_ip_header = "cf-connecting-ip"`: Cloudflare's edge sets it to the visitor's address on every request (a value the
  visitor sent is replaced). Any header other than `x-forwarded-for` is read as one address; a missing, repeated or malformed
  header falls back to the peer address. IPv6 visitors are keyed by their /64 (`rate_limit.ipv6_prefix_bits`).
* Keep `listen` on loopback: the API must not be reachable around the tunnel. (Any program on this PC could also connect and set
  the header; on a single-user machine that is acceptable.)

**CORS.** The app is served from the Pages origin (`https://kob-web.pages.dev`, a custom domain), the API from
`https://chart.example.org`: the browser reads the API's answers only when they carry `Access-Control-Allow-Origin`. The API
sends no CORS header unless `api.cors_allow_origin` is set; then every answer carries it (with `GET, OPTIONS` and the
`content-type` header) and a preflight `OPTIONS` is answered 204. Use `"*"`: the data is public, the API uses no cookies or
credentials, and one value cannot name both the `pages.dev` address and a custom domain; `"*"` also lets other deployments of
the app, a user's own build and third-party tools read it, which is what a neutral indexer should allow. The WebSocket is not
subject to CORS (the API checks no `Origin`).

**Caching.** The API sends no `Cache-Control` header and its paths have no file extension, so Cloudflare caches nothing by
default (`cf-cache-status: DYNAMIC`): every answer is live. That is required for the books, orders, depth, stats, trades,
fills, health, tokens and holdings, and for the WebSocket, which must never be cached. The API already keeps the hot reads
(candles, stats, books) in a response cache per data generation of its store, so repeated chart loads cost the database little.
Edge caching of the chart endpoints (`/v1/candles/{token}`, `/v1/pairs/{base}/{quote}/candles`) is optional and **not
recommended on the free plan**: a Cache Rule that overrides the TTL cannot go below 2 hours there (2 hours of stale candles), and
a shorter TTL would need the API to send `Cache-Control` on those answers, which it does not. If a paid plan or a later API
change makes a short TTL possible: cache only those two paths, keep the query string in the cache key (`interval`, `from`,
`limit`), and a TTL of a few seconds.

**Abuse, in the API and at Cloudflare.** The API limits each client on its own: 20 requests per second with a burst of 60 per
client address (an IPv6 visitor by its /64, and all the /64s of one /48 together at 100 per second), a heavy read (candles, stats,
depth, trades, books, holdings, strays, pairs) costs 5 of those tokens, and the whole server answers 500 requests per second (burst
1,000). WebSockets are capped per client address and in all (`docs/api.md`). Past a limit the answer is 429 with `Retry-After`. Two
consequences: visitors behind one carrier-grade NAT address share one bucket (a market list reads one `/v1/stats` per token), and a
flood spread over many addresses can drain the global bucket so that everyone gets 429. If that happens, add rules at Cloudflare in
front of the tunnel instead of raising the limits: a rate-limiting rule for the heavy paths (`/v1/candles/`, `/v1/stats/`,
`/v1/depth/`, `/v1/trades/`, `/v1/books/`, `/v1/pairs`) and Bot Fight Mode. What the free plan allows changes; check the dashboard.

## 4. Data directory, size and backups

`kob-data` holds the SQLite database (`index.sqlite3`) and the record log (`records/`). The record log is the asset to back
up: it is append-only, all but the newest segment never change, and it is the only source of the history once the node has
pruned it. The database can always be rebuilt from it (`kob-executor index replay`). Details, a consistent database copy while
the indexer runs, and restoring: `docs/ops/executor.md`, Part B, section 8.

**Size.** The design estimate was 2.6 KB of database and 1 KB of log per order. The testnet-10 soak measured more, because an order
also carries its events and fills: 1.6 GB of database for 295,000 orders and 2.85 GB for 501,000 (5.4 to 5.7 KB per order), and a
record log of 1.2 GB for 306,000 orders (3.9 KB per order). Plan on **about 10 KB per order in all**, plus a constant 42 MB window
of chain blocks, and keep the drive at least half empty (the disk alarm of `executor.md` Part B 6 fires above 70 %). How many
orders a day mainnet will bring is not known yet: read the growth of `kob-data` after a week and extrapolate.

**Time to restore** (testnet-10 drill of 2026-10-20, `docs/testing/TN10-TEST-2026-10.md` section 14): `VACUUM INTO` of the running
2.85 GB database took 84 seconds; restoring that copy next to the record log and bringing it forward (`replay --onto-database`)
took minutes; a full `index replay` of a 1.2 GB log (367,916 frames, 306,552 orders) took 2,634 seconds (about 44 minutes) and
prints nothing until it is done. The chart is dark that long if you rebuild in place; section 6 shows how to avoid that.

**Backups on a Windows home PC.** The recipe of `executor.md` 8 uses the `sqlite3` command line for `VACUUM INTO`, which Windows
does not ship. Either download SQLite's command-line tools from sqlite.org, or skip the database: it is rebuildable from the log, so
the log is the backup that matters. Copy it with a scheduled task (a segment never changes once the next one has started):

```
robocopy C:\kob\kob-data\records D:\kob-backup\records /E /R:1 /W:1
sqlite3 C:\kob\kob-data\index.sqlite3 "VACUUM INTO 'D:\kob-backup\index-2026-11-01.sqlite3'"      # optional; the target must not exist
```

(Without `sqlite3`: stop the indexer and copy `index.sqlite3`; a plain file copy while it runs is not consistent.) Keep the copy on
another drive or machine, and encrypt anything that leaves the house. Also back up the tunnel's `~/.cloudflared/<id>.json` (the
credentials), its `config.yml` and the `cert.pem` that `cloudflared tunnel login` wrote. Losing them loses no data: a new tunnel
and a new `route dns` bring the hostname back, but until then the app has no indexer.

**Sleep, updates, reboots.** A PC that sleeps or hibernates serves nothing: set the power plan to never sleep. Windows Update may
reboot it: set active hours, and run both the indexer and `cloudflared` as start-at-boot tasks (section 2, step 5;
`docs/ops/executor-quickstart.md`, section 6) so they come back by themselves. An outage longer than the node's retention (7 days
with the flags of the quickstart, about 30 hours by default; public nodes do not state theirs) no longer halts the follower: it moves
the store to the node's pruning point on its own and reports it as `gaps` in `/v1/health`, but orders created inside the skipped
stretch stay invisible (Part B, 7.4). To keep the history whole after a long outage, restore a backup or a second site (7.4, options
1 and 2).

## 5. What to watch

* The status line in the log every minute: `status: indexer following, caught up (N DAA behind the node), ...`; with public
  nodes it ends with both hosts and their state. `WARNING:` names what needs you.
* `GET /v1/health` (locally `http://127.0.0.1:8090/v1/health`, publicly through the tunnel): `state` (`following`,
  `catching_up`, `node_unavailable`, `gap`), `lag_seconds`, `alarms`, `book_complete`, and while catching up `catch_up` (pace and ETA).
  `GET /v1/health/ready` answers 200 when the store is following **or catching up with a lag below the first alarm threshold (1 hour by default)**,
  and 503 otherwise (a first sync with a day of lag, a node that is unavailable, a gap): a monitor pointed at
  `https://chart.example.org/v1/health/ready` therefore stays green for a store that is up to an hour behind. For "caught up" read
  `state = following` of `/v1/health`.
* Page yourself on `state = gap` (the indexer halted: Part B, 7.4) and on `node_unavailable` for more than a few minutes; the
  full alarm table is Part B, section 6. Nothing in KOB sends alerts, so set up one of these (free):
  * **Cloudflare Notifications** (dashboard, *Notifications* > *Add*): choose the alert for a Cloudflare Tunnel that is down or
    unhealthy (the exact name in the list varies; look for *Tunnel*), delivered by e-mail. It tells you the PC or `cloudflared` is off.
  * **An external uptime monitor** (any service that fetches a URL every few minutes and matches a keyword) on
    `https://chart.example.org/v1/health` with the keyword `"state":"following"`. It catches the tunnel, the PC and a stuck
    indexer at once. A second check on `/v1/health/ready` gives you the status code (200 or 503, see above).
  * **A local check**, a scheduled task on another machine of yours: `curl -sf https://chart.example.org/v1/health/ready`, and send
    yourself a message when it fails.
* `cloudflared` logs its connections; the dashboard (*Zero Trust* > *Networks* > *Tunnels*) shows the tunnel healthy or down.
* The disk of `kob-data` (a few GB) and of the node.

When this server is down, the hosted app still loads and users' funds are safe (everything is on chain). Cancelling an order still
works, from the node and the records kept in the browser. The app cannot show the order book, charts, trades or order status from
it, and it refuses to place a new order until an indexer answers. Users can switch to another indexer in Settings; listing
third-party indexers in the deployment helps them.

## 6. Upgrading, and rebuilding without chart downtime

**A new release of the program.** Read the release notes (`docs/ops/release.md`, 7) for the payload version and for whether the
database is brought up to date in place or rebuilt. Then:

1. `kob-executor --version` and `curl -s http://127.0.0.1:8090/v1/health` (`version`, `commit`, `payload_version`) say what runs now.
2. Download the new archive, check its hash (`executor-quickstart.md`, 1) and unzip it into a new folder next to the old one.
3. **In place, with a short outage.** Stop the indexer gracefully (Ctrl+C, `systemctl stop`, the scheduled task's window; it flushes
   the database and exits within seconds), put the new `kob-executor` where the old one was (keep the old as `kob-executor.old`) and
   start it. The chart is dark for the stop and the start only: in the soak test the API answered 3 seconds after a start and the
   store was following again 35 seconds after a 6-minute outage. A database the release brings up to date in place does it at
   that start (7 seconds on a 1.6 GB copy). Check `/v1/health` again: `version`, `commit`, `payload_version`, `state = following`.
4. **Rebuild beside the running one, then switch.** When the release needs a rebuild (`index replay`: 44 minutes per 1.2 GB of log
   here, offline, no API), or you want to see the new version work before the public does:
   1. Make a second folder `B` (for example `C:\kob-b`) from the new archive. Copy the running folder's `kob-executor.toml` into it and
      change only the port: `listen = "127.0.0.1:8091"`. `data_dir = "kob-data"` is relative to the file, so `B` has its own data.
   2. Copy the record log: the whole `kob-data/records/` folder of the running indexer into `B/kob-data/records/` (a half-written last
      frame in the copy is dropped when the log is opened). Then run `kob-executor index replay --config kob-executor.toml` in `B`: it
      builds a new database from the log under the new program's rules and prints a summary at the end (rejected placements by
      reason). Faster, when the release does not change how the database is derived: put a `VACUUM INTO` copy of the running
      database (section 4) in as `B/kob-data/index.sqlite3` and run `index replay --onto-database` instead (`executor.md`, 8).
   3. Start `B` (`kob-executor index --config kob-executor.toml`). It catches up the time since the last logged frame from the node.
      Wait until `curl -s http://127.0.0.1:8091/v1/health` shows the new `version` and `commit`, `state = following` and
      `book_complete = true`, and compare a few answers with the running one (`/v1/tokens`, `/v1/orders?...`).
   4. **Switch.** Point the tunnel's service at `http://127.0.0.1:8091`: for a dashboard-managed tunnel edit the public hostname; with
      a `config.yml` edit it and restart `cloudflared` (a few seconds). Open WebSockets reconnect by themselves, and the app polls
      every 5 seconds while its socket is down. Check `https://chart.example.org/v1/health`.
   5. Stop the first indexer. Keep its folder for a day as a way back, then delete it. `B` can stay on port 8091 for good; make `B`
      the folder your start-at-boot task runs.

   Both indexers read the same node meanwhile, which a node allows. Never point two indexers at the same data directory: the
   second is refused with a message naming the first.

## 7. A testnet-10 server and a mainnet server side by side

One binary serves one network (`executor-quickstart.md`, 1), and the web app's deployment is one network too. To serve both from one
machine you need a second of everything: a second folder with the other network's archive and its own `kob-data`; its own API port
(`8090` for one, `8091` for the other); its own hostname (`chart.example.org` and `chart-main.example.org`, each with its own
`hostname:` / `path: ^/v1/` rule before the 404 rule of section 2); and its own node. The public resolver pair works for both; an own
mainnet node (`ws://127.0.0.1:18110`) needs the hardware of `release.md`, 8 (8 cores, 16 GB RAM, a 640 GB SSD). Each server needs its
own start-at-boot task and its own backups. On the app side the deploy workflow has one set of variables, so a second Pages
project (another `KOB_PAGES_PROJECT`, `KOB_PAGES_NETWORK=mainnet` and its indexer URL) is a hand deploy (`web-hosting.md`, 6).
