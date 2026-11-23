# Token-logo votes

Status: built on branch `logo-votes` (2026-11-20), on `ci/kobd-left`. The design is the founder's (2026-11-20). Code:
`crates/kobd/src/logo_votes.rs` (with `logo_votes.sql`), the routes in `crates/kobd/src/api/rest.rs`, the web side in
`web/src/data/logo-votes.ts`, `web/src/ui/market/LogoReport.tsx` and `use-token-logo.ts`. Routes: `docs/api-reference.md` 3.19b.

Any wallet can vote up or down on a token's logo. The voter does not need to hold the token. A vote is a message the wallet signs:
no transaction, no fee. A logo is judged once at least 3 addresses voted on it, and a net score of -1 or lower then hides it by
default. Reports of illegal images stay separate (3.19): free, unsigned, they hide at once for the operator to review, with at most
20 unreviewed hides. Votes do not change that path.

## 1. Endpoints

| Route | What |
|---|---|
| `POST /v1/logo-votes` | casts one vote: `{covenant_id, vote: "up"\|"down", address, day, nonce, signature}` |
| `GET /v1/logo-votes` | the totals per token, the tokens the rule hides, the rule and the work: `{logos: [{covenant_id, up, down}], hidden, min_voters: 3, hide_at: -1, pow_bits, interval_secs, updated_ms}` |

Both exist only with `[logo_votes] enabled = true`. The GET is rendered at most every 5 s (a vote drops it) and served with
`Cache-Control: public, max-age=5`. It never names a voter, only the two counts per token.

## 2. The message

The wallet signs this text, one field per line, LF, no trailing newline:

```
KOB logo vote
Token: <covenant id, 64 lower-case hex>
Vote: up
Address: kaspatest:qq...
Network: testnet-10
Day: 2026-11-20
```

The signature is Kaspa's personal-message standard, the one rusty-kaspa's `wallet/core/src/message.rs`, KasWare's `signMessage`
(0.11.5) and Kaspire's `signMessage` (0.5.31) use: BIP340 Schnorr over BLAKE2b-256 of the UTF-8 text, keyed with
`PersonalMessageSigningHash`. kobd builds the same text from the request fields, so it never reads free text, and verifies the
64-byte signature against the x-only key of `address` (a Schnorr P2PK address of the indexer's network; P2SH and ECDSA addresses
cannot vote). `Day` is the UTC date; kobd takes today and yesterday, so a vote signed just before midnight still counts. The web app
asks KasWare for `{type: "schnorr"}`, since its `auto` picks ECDSA for a Tangem account. The private-key signer and Kastle have no
message signing in the app, so they cannot vote yet.

## 3. Proof of work

Hashcash over (network, token, address, day, nonce):

```
SHA-256("KOB logo vote pow/1\n" + network + "\n" + covenant_id + "\n" + address + "\n" + day + "\n" + nonce)
```

must have `pow_bits` leading zero bits. `nonce` is decimal (at most 20 digits, a `u64`). The server sets the difficulty
(`[logo_votes] pow_bits`, default **20**, at most 32) and publishes it in the GET. 20 bits is about a million hashes: noble's
SHA-256 in V8 does about 500 000 a second with the prefix hashed once, so about 2 s on a desktop and several seconds on a phone. The
page hashes in slices and yields between them. The work runs while the wallet shows the message, so the two waits overlap. The
direction is not in the hash: the work buys one vote of this address on this token on this day.

## 4. The history rule

The read API never asks the node (`api/mod.rs`: public load must not reach it), so the rule reads the store. The voting address's
key must have **KOB history at least `min_history_secs` old** (default 7 days) **and older than the token** (founder decision
2026-11-23, audit C-M3): an order it placed (`orders.maker_id`) or a token output it held (`token_holdings.owner_id`). Its age is
the newest block's DAA score (`blocks`) minus the first such row's DAA score, divided by `daa_per_second`. The token's creation is
its KCC-23 genesis DAA, else the first of its token outputs or of the orders that trade it the store holds; the first history must
come before it. A key made for the vote, or one first funded after the token appeared (the cheap sybil of C-M3), cannot vote on
it. Index seeks only (`orders_maker_genesis`, `token_holdings_owner_page`, `token_holdings_token_page`, `orders_token_genesis`,
`orders_quote_genesis`). A pruned row only makes the history, or the token, look younger.

**Or a holder of the token** (founder decision 2026-11-23, audit R2C-M3): the key may also vote on a token's logo when it has held
that token without a break for at least `min_history_secs`. Its holdings of the token (`token_holdings` by `owner_id`, the newest
512 rows) form lines: a holding spent by a transaction that creates another holding of the same owner and token (a merge, a send
to itself, a partial sale that leaves the rest) continues its line; one that arrived any other way starts a new one. The line of
each holding it holds now starts at the oldest holding it continues, and the oldest such start is when it began holding. Tokens
received today (dust) count from today, and tokens bought back after selling everything count from the buy. A pruned row (a lean
store) breaks a line, which only makes the holding look younger, and two holdings of one line created in the same block may not be
linked (strict). This lets holders defend their own token's logo; hiding a token's logo now needs a week of holding it on several
addresses, or keys with KOB history from before the token.

What remains (residual): a token younger than 7 days can only be voted on by addresses whose history is older than it and at
least 7 days old, so in its first week only such addresses decide its logo, and a stockpile of keys prepared 7 days ahead can
still hide the logo of any token created after it (R2C-M3) until its holders have held it for a week and vote it back.

What it costs a sybil: each fake address needs an order or a token output on chain (a transaction and its storage mass) and must
then wait 7 days. A wallet that only ever held KAS and never touched KOB cannot vote (an open question below).

## 5. Limits

| Bound | Default | Where |
|---|---|---|
| one vote per wallet across all logos | per 24 h (`interval_secs = 86400`) | the newest `logo_votes` row of the key; `429 too_soon` with `Retry-After` |
| the same vote again | answered `unchanged`, uses nothing | |
| votes per client address per hour | 20 (`per_client_per_hour`) | the client key of the guard (IPv6 per `ipv6_prefix_bits`), as the reports |
| votes from all clients per 24 h | 20 000 (`max_per_day`) | |
| origins | `[logo_votes] origins`, else `[logo_reports] origins`, else a single `cors_allow_origin` | a browser page of another origin is `403` |
| media type, body | `application/json`, at most 1 KiB, unknown fields refused | |
| rate-limit weight | `heavy_route_cost` (default 5) for the POST | the guard, before the handler; the GET costs 1 |

"One vote per wallet per 24 hours" (the founder: "the interval is 24 hours") is read as one vote per wallet across all logos per 24 hours, not
per logo. A vote can be changed after the interval: a new vote on the same logo replaces the old one in the totals. The checks run
cheapest first: the day, the work, the signature, then the store (the token is indexed, the history, the interval) and the limits.
Only an accepted vote counts against the limits.

## 6. Storage

`crates/kobd/src/logo_votes.sql`, in the indexer's database next to the report tables:

* `logo_votes`: every accepted vote for good (the founder: keep everything): covenant id, voter key, +1 or -1, the day and nonce, the
  64-byte signature (so each row can be verified again), the time and `counted` or `changed`. Indexes `(voter, created_ms)` for the
  interval and `(covenant_id, voter, id)` for the voter's current vote.
* `logo_vote_totals`: up and down per token, each voter's newest vote counted once, kept in the vote's transaction.

The tables come the way `book_quotes` came within schema 14: `CREATE TABLE IF NOT EXISTS` at every open (`db::open_writer`,
`db::open_memory`, `logo_votes::open`), no migration run and no change of `SCHEMA_VERSION`. A schema bump would refuse every schema-14
store until an offline `kobd db migrate` rebuilt it, for a side table that is not derived from the chain. `kobd db migrate` copies and
verifies both tables, and `logo_votes` keeps its AUTOINCREMENT sequence. `kobd db replay` does not rebuild them:
`kobd logo-reports export` now writes `kob-logo-reports/2` with a `votes` list, and `import` loads it (and still takes `/1`) and
recounts the totals.

## 7. The web app

* `GET /v1/logo-votes` is read at start and every 2 minutes, next to the blocklist, and kept in localStorage. It fails open.
* Standalone mode asks the indexer nothing for votes: the reads pause (the kept totals still apply) and the Logo card shows no vote
  buttons. Only the report blocklist is still read there.
* The rule (`verdictOf`): fewer than `min_voters` voters, not judged; else hidden at a net score of `hide_at` or lower.
* Settings, "Token names and icons", "Logos voted down": **Show logos at 0 and above** (default) or **Show all logos**
  (`kob.icons.votes`).
* A hidden logo is not requested. The icon shows its letter with a small mark, titled "Hidden by votes. Click to show it once."; a
  click shows it for the rest of the visit. The token's Logo card says "Hidden by votes" with a **Show once** button.
* The Logo card shows the two counts on the thumbs up and down buttons. A vote needs a connected KasWare or Kaspire wallet on the app's
  network.

## 8. Open questions for the founder

1. Wallets with only KAS and no KOB history cannot vote. Widening it means asking the node (`getUtxosByAddresses`, an aged balance),
   which the read API does not do today.
2. With 3 voters a net -1 hides a logo: three young-but-day-old addresses with a KOB output each can hide one. Raise `min_voters`
   later, or weight the votes by history?
3. Votes are per token. A logo the creator cannot change (the KCC-23 declaration) keeps its votes; kascov.io's listed image can change
   under them. Per-logo-version totals would need the image hash in the message.
4. `pow_bits` 20 is a few seconds on a phone at best; tune it once real devices are measured.
5. No vote retraction: a voter can only flip after the interval.
