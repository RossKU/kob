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

## 4. Data directory and backups

`kob-data` holds the SQLite database (`index.sqlite3`) and the record log (`records/`). The record log is the asset to back
up: it is append-only, all but the newest segment never change, and it is the only source of the history once the node has
pruned it. The database can always be rebuilt from it (`kob-executor index replay`). Details, a consistent database copy while
the indexer runs, and restoring: `docs/ops/executor.md`, Part B, section 8. A home PC that is off for longer than the node's
retention needs the gap runbook (Part B, 7.4) before it serves again.

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
  full alarm table is Part B, section 6.
* `cloudflared` logs its connections; the dashboard (*Zero Trust* > *Networks* > *Tunnels*) shows the tunnel healthy or down.
* The disk of `kob-data` (a few GB) and of the node.

When this server is down the hosted app cannot show books or charts from it. Users keep their funds (everything is on chain)
and can switch to another indexer in Settings; listing third-party indexers in the deployment helps them.
