# Token registry

A pinned snapshot of facts: the token programs this build supports and, per token, facts anyone can re-derive from chain data.
Validated by `kob_protocol::registry` (`Registry::parse` / `validate`) against the structural schema `tokens.schema.json` plus
cross-field rules the schema cannot express. Nothing here is on-chain and nothing here is a judgement about a token: KOB makes no
listing decision, and whether a token trades depends only on technical facts (its program is one this build supports, and the chain
data its orders pin). The order contracts do not read this file at all (an order pins its own covenant id and template hash).

| File | What |
|---|---|
| `tokens.json` | The registry shipped with a build (mainnet). It pins seven token programs (reference KCC-20 3/3, the program KOB issues; the 8/8 prototype, kept so that testnet tokens issued with it stay readable; the reference's published public-mint build `kcc20-ref-public-mint`, the two KRON programs, KaspaCom's KCC20 0.2.5, KaspaRocket's 10 / 10 build of the public-mint actor `kasparocket-token-11458`), five of which this build supports for trading: the two KRON templates (internal engineering review B2, 2026-09-30, with the conditions recorded in their `risks`: a per-token genesis check and no live minter, both of which the data model now reports as facts, the `genesis_unverified` and `live_mint_authority` warnings), the reference KCC-20 3/3 (`kcc20-ref-3x3`, internal review only, maintainer decision 2026-10-12, canary launch) and the reference's published public-mint build `kcc20-ref-public-mint` (internal review only, maintainer decision 2026-10-13: KOB's second issuance option and the program of third-party launchpads such as Kasseal; a genesis `PublicMint` bounds growth at `supply + mint_allowance`) and KaspaRocket's `kasparocket-token-11458` (internal review only, maintainer decision 2026-10-27: the same actor at 10 / 10 slots, supply fixed at genesis; its tokens declare no decimals and take the program's default, 3, the KaspaRocket convention). The 8/8 prototype and KaspaCom's are `pending-review` (not supported for trading), KaspaCom's with finding K-1 confirmed by an engine PoC (the creator of a mint_policy 2 token can mint past max_supply or plant a non-program covenant UTXO through `set_public_mint_active`; see its `risks`). It carries entries for the KRON family of the 2026-09-29 census (template A `2ed46a7e...`: KRON, KASCOV, PEPE, ANSEM, IFWEN, PEEPS, DNBT; template B `8097c96f...`: KDIST): their identity (`verified`), the decimals / ticker / name snapshot and, from the genesis check of 2026-10-03 on mainnet data, a `genesis` record each (every genesis output an instance of the program, no live mint authority; re-derived from `evidence/mainnet-genesis.json` by `kob registry verify-genesis`). An entry is facts only: it neither makes a token tradable nor says that it is genuine. The file is the release's pinned registry: its sha256 is in `contracts/deploy/mainnet/deployment.json` (`scripts/build-deploy.sh mainnet`, `docs/ops/release.md`). |
| `evidence/mainnet-genesis.json` | Chain evidence behind the `genesis` records: each token's genesis transaction, the redeem scripts of its genesis outputs and the node's liveness view, collected read-only by `web/scripts/registry-genesis-evidence.mjs`. Untrusted input: the verifier checks every byte against a hash. |
| `tokens.schema.json` | JSON Schema (draft 2020-12): structure, types, enums, hex patterns, `additionalProperties: false`. |
| `tokens.example.json` | Fictional TN10 entries (one `kcc20`, one `kron`; covenant ids are patterns, not deployed tokens). Format example and test fixture; unlike `tokens.json`, every template in it is marked `reviewed` so that the paths of a supported program can be exercised for each one. |

## Curation is not KOB's

KOB is not the authority on tokens (founder principle 2026-10-19). Facts live on chain, and this file only pins a snapshot of them;
curation (an official status, anti-impersonation, logos, warnings written by people about a token) belongs to external community token
lists that the user chooses, in the style of Uniswap Token Lists. A future release may let a user reference such a list by its pinned
hash; nothing of that is implemented (feature freeze). Until then KOB states no opinion of any token: there is no `official` flag, no
listing status and no free-text warning in this format, and the risk a UI shows is derived from facts by code (the template
`capabilities`, the warnings below), never written per token by a maintainer.

## Do I need a registry entry? (token issuers)

No. A token of a supported program trades without one: the chain carries what an app needs. The name, ticker and decimals an issuer
chooses are recorded on chain with the token (KCC-23, `docs/spec/token-metadata.md`), and apps that read that record, KOB's web app
and `kob` among them, show them marked as the creator's own claim. The "Token facts (registry format)" entry that the issue form
hands back is optional output for anyone who wants to keep or publish those facts; there is no place to submit it, and mainnet
registry changes happen only in a release (`docs/ops/release.md`, 3).

## Two lists: supported programs, open tokens

* **`templates` is the program allowlist.** Only token programs marked `review_status: reviewed` are supported for trading by this build:
  the engineering review of the program is done (owner types, no admin path, conservation, limits, the order suites on its holders)
  and the build embeds it. This is a statement about a token PROGRAM, never about a token. In the shipped `tokens.json` five programs
  have that state: the two KRON programs, after an internal review with conditions, the reference KCC-20 3/3, KOB's standard program,
  by internal review (maintainer decision 2026-10-12), the reference's published public-mint build, by internal review (maintainer
  decision 2026-10-13), and KaspaRocket's 10 / 10 build of it, by internal review (maintainer decision 2026-10-27). No independent audit has been done; the other KCC-20 programs (the 8/8 prototype, KaspaCom's KCC20 0.2.5) are
  `pending-review`: they are template entries too, but no token of them trades until this build supports them. Because the token list
  is open (below), every token on a supported program is shown and tradable, with the `genesis_unverified` warning until its entry
  records a clean genesis check. A program is accepted
  because its template hash is here, never by code special-casing: a third-party KCC-20-compatible program (same 112-byte state,
  draft transfer tags, owner scheme 4) such as KaspaCom's KCC20 0.2.5 is one more template entry plus its pinned artifact. Adding one
  is data plus review: the artifact or raw bytes under `contracts/third-party/` (with provenance and licence), its profile in
  `crates/kob-protocol/data/programs.json` (`kob registry add-program` derives it from the bytes and writes the test stub), its compute
  budgets and keeper tips (`KOB_REGEN=1`), the engine tests and the reviewer checklist of
  [`docs/spec/token-programs.md`](../docs/spec/token-programs.md). A template entry whose program this build embeds must state
  the profile's family, layout, slots and custody owner (the validator checks it).
* **`tokens` is the OPEN list.** Every token whose program this build supports is tradable and shown by the executor, whether or not it
  has an entry here (an order pins its own covenant id and template hash; the executor shows the token when the hash is a supported
  program, from the first order placed on it). Without an entry nothing states the decimals (the chain records none): clients take
  them from the token's KCC-23 declaration or the scale its orders use, and the first order names them (the web app asks for them;
  `kob place --decimals N`). The web app opens the market of a KCC-20 token no order names yet from the wallet's own node-verified token
  outputs of it, which show its program and extension commitment. An entry adds facts only: the decimals / ticker / name snapshot, the
  identity check (`verified`) and the genesis record; it is not a listing, and a token without one trades the same way. Tokens are
  identified by covenant id plus template hash: tickers collide (a KRON-launchpad "NACHO" is unrelated to the KRC-20 NACHO), so UIs
  always show the short covenant id next to the ticker.
* **`capabilities`** on a template label what its authorities can do beyond owner-authorised transfers: `mint-authority`, `public-mint`,
  `burn`, and `freeze`, `seize`, `blacklist`. KOB has no special mechanism for frozen or seized tokens (the token program simply
  rejects the transaction); a template with freeze, seize or blacklist is labelled in the registry, the API (`powers`) and every UI, and an
  order whose engine pre-simulation fails is flagged `possibly_frozen` instead of being listed as live.

## Format

```
{ schema_version: 1, network: "mainnet" | "testnet-10" | "devnet", templates: [...], tokens: [...] }
```

**Family** = a token program lineage KOB can escrow: `kcc20` (draft KCC-20, 112-byte state) or `kron` (KRON, 46-byte state). One
order-contract pair per family; a token of one family can never trade through the other family's orders.

**`templates`** (pinned token programs; hashes are recomputed from the committed artifacts/bytes by `crates/kob-tests/tests/registry_tests.rs`):

- `capabilities` (optional list, default none): `mint-authority`, `public-mint`, `burn`, `freeze`, `seize`, `blacklist`
- `risks` (optional list of strings, default none): declared risks that are not a capability (missing hardening, signing assumptions)
- `id`, `family`, `template_hash` (silverscript blake3 template hash, lowercase hex), `prefix_len`, `suffix_len`, `state_len` (112 / 46).
  A program whose state opens with compiler-owned context fields (`kcc20-ref-public-mint`: `gen__kcc20_template`, always the
  program's own Sil template hash) is pinned as its KCC-1 actor-type handle: the context belongs to the prefix (34 B), the
  open state is the 112-byte KCC-20 state, and `template_hash` is the handle's (what every order pins)
- `max_token_inputs`, `max_token_outputs`: slot limits of the program (kcc20 reference 3/3, the 8/8 prototype 8/8, KRON 4/5)
- `escrow`: owner types the order contracts use. kcc20: `owner_scheme` 4 (covenant id) and `borrow_scheme` 0. kron: `id_type` 2,
  `is_minter` 0 and `delivery_id_type` 3 (address presence, KRON-wallet compatible; 0 = pubkey is the alternative)
- `review_status`: `pending-review` | `reviewed` (`reviewed` = this build supports trading the program; its review record is an
  engineering note); `source`: path of the pinned artifact/bytes, upstream provenance

**`tokens`** (facts only):

- `ticker` (2-12 ASCII `A-Z0-9`), `name`, `family`, `covenant_id` (hex32), `template_id` (must exist and match the family)
- `extension_commitment` (hex32, required for kcc20, `null` for kron) and `extension_class` (`none` | `fixed-supply-standard` | `other`;
  `none` means an all-zero commitment on kcc20, and is the only class for kron)
- `decimals` (<= 18; an order's `scale` is `10^min(decimals, 9)`). Ticker, name and decimals are a pinned snapshot until the app reads
  on-chain metadata for every token (KCC-23 declarations already take precedence where a token has one, see docs/spec/token-metadata.md).
  Protocol v3 orders have no lot and no price tick: the legacy fields `lot_size` and `tick` of older files are read and ignored (this
  file no longer carries them)
- optional `max_token_inputs` / `max_token_outputs` (must equal the template's)
- `verified` (required boolean): the covenant id and template hash were checked against a chain UTXO of the token (the UTXO is an
  instance of the template). A fact anyone can re-derive, not an endorsement
- optional `genesis_verified` (genesis check, see below: `true` = every genesis output checked and clean, `false` = checked and NOT
  clean, absent = not checked) and optional `genesis` (the record behind it, see below)
- optional `display` (`description`, `website` https, `icon` https/ipfs, `x` and `telegram` the token's accounts as https addresses (the
  Create token form writes `https://x.com/<handle>` and `https://t.me/<handle>`), `kcc23` the token's KCC-23 declaration object, a copy of
  what the token recorded on chain at its creation: the app reads the facts from the chain itself and flags an entry whose `ticker`,
  `name` or `decimals` differ, see docs/spec/token-metadata.md). `display` is a snapshot of what the token itself declares, or neutral
  facts (its launchpad, its program); it carries no judgement of the token
- Older files' `status` (listing state), `official` and `warning` (a maintainer's text) are legacy: read and ignored, whatever they
  hold (`kob_protocol::registry::LEGACY_TOKEN_KEYS`, and the web app's validator alike), never honoured and never written, so an
  operator's registry of the earlier shape (a testnet file, say) keeps loading and trades exactly what it would trade without them

## Rules the validator enforces

- Identity is `(family, covenant_id, template hash, extension_commitment)` (`Registry::allowlist_key`); identities are unique, and so is each
  covenant id (one covenant id = one token). Asset identity for order books is the same tuple (fungibility only among equal
  extension commitments, as discussed in the KCC-20 thread on kas-smiths.org).
- Unknown fields anywhere are an error; hex is exactly 64 lowercase characters; unknown network / template / family mismatch are errors.
- `genesis_verified: true` requires `verified`; a `genesis` record requires `genesis_verified`; the record's own fields are checked
  (strictly increasing outputs, minters among them, non-negative supply and allowance, `txid:index` live minters, a printable source).
- Tickers are ASCII uppercase alphanumerics of 2 to 12 characters. They are not unique: two entries (two covenant ids) may share a ticker
  or use look-alike ones (`KRON`, `KR0N`), and neither is refused. Choosing which of two tokens may carry a ticker would be a curation
  decision, and KOB makes none (curation belongs to the community lists a user chooses); the covenant id is the identity. Names may not
  contain control, zero-width or bidi characters.

## Display

`display_name(token)` gives `TICKER (abcd…1234)`: ticker and the first and last four hex digits of the covenant id, for tools and logs
(the same as the web registry's `displayName`). Neither shows a standing, because there is none. When two tokens the app shows share a
ticker (look-alike letters and homoglyphs count as the same) the KOB web app says "Another token uses this ticker — check the covenant
id" (`web/README.md`, "Trust boundaries"); the issuance form shows the same as a warning when the new ticker matches or resembles one
in the registry. The validator accepts both entries of a second "PEPE": each is a fact about its own covenant id.

## Relation to the launch allowlist

The allowlist row is `(covenant id, token template hash, suffix length, extension_commitment, decimals)` plus the family and slot
limits: `Registry::allowlist()` returns exactly that for every token entry whose program this build supports. `kob-executor --tokens
registry/tokens.json` reads this file directly (validated): its supported programs open the token list, and its entries add their
facts (decimals, so the standard scale; template hash and extension commitment, which an order of the token must match). Protocol v3
has no lot and no tick (orders are any amount in base units, prices per whole token). Operators may load another file; the release
pins this one by hash (the deployment record's `registry` section: `path`, `sha256`, the supported and pending templates, and
`tokens` [{`ticker`, `covenant_id`, `genesis_verified`}]).

## Genesis check

A KIP-20 covenant id commits to the genesis outpoint and to the index, value and script public key of every output of the genesis group,
and nothing else: for P2SH outputs only to the hash of the redeem script. A token program reads balances and authority from any input
carrying its covenant id, so one hidden non-template output in a token's genesis (a "backdoor" script) can later mint look-alike tokens or
feed a fake balance into a real transfer, and neither the template nor a holder UTXO shows it. `kob_protocol::registry::verify_genesis`
recomputes the covenant id from the authorising outpoint and the outputs given (so they must be the complete group) and requires every
output to be the P2SH of a revealed redeem script that is an instance of the token's pinned program with a decodable state; it returns
the output count, the supply and the outputs that carry a mint authority. Record the result as `genesis_verified: true` (clean) or `false`
(not clean: the data model then warns `genesis_unverified`), and the facts as the token's `genesis` record:

```
genesis: { txid, daa_score, outputs: [..], supply, minter_outputs: [..], mint_allowance (optional), issuer (optional),
           live_minters: ["txid:index", ..] (optional), checked_at_daa, source }
```

`mint_allowance` is recorded for a token of the published public-mint build (`kcc20-ref-public-mint`) whose genesis holds
`PublicMint` minters: the sum of their `remaining` allowance (non-negative, and only with non-empty `minter_outputs`).
`supply` is what the genesis holders carry; the token's maximum supply is `supply + mint_allowance`, fixed at genesis (a mint
moves allowance into a holder, a split or merge of minters keeps their sum, and only a `PublicMint` creates a `PublicMint`),
and the amount minted so far is that maximum less the `remaining` of the live minters (`kob_protocol::public_mint`).

`issuer` names the upstream build of the public-mint app (argent-lang/kcc20-reference) the genesis's `PublicMint` / `TokenSeed` actors
are instances of: `kcc20-reference@c8a0871` or `kcc20-reference@27f8403` (the latter adds the owner-signed `rebalance_value` entry to both
issuers, which changes neither the supply nor any allowance, see `docs/spec/kcc-conformance.md` 4.3). Both builds share one `KCC20` holder
program (`kcc20-ref-public-mint`); a genesis never mixes the two builds, and a genesis of holders only has no `issuer`. `verify-genesis`
derives it and prints it.

`live_minters` is condition C2 of the KRON review: `[]` = no live mint authority, absent = not determined, non-empty = those cells
can still mint (the data model warns `live_mint_authority`). For the KRON programs it follows from the genesis: only a transaction that
spends a minter can create an `is_minter` output (every non-minter token input refuses one, `review_b2_kron` r_kr_01 / r_kr_15), and a
covenant id's only other source of outputs is its genesis, so a genesis without a minter output proves the token never has a live
minter. A genesis with one needs the minter's lineage traced to its live cells. The same holds for the reference KCC-20 programs KOB
pins (the standalone 3/3 and its prototype variants, the published public-mint build and KaspaRocket's 10 / 10 build of it): a holder transfer creates only holders, so a
genesis of holders (and seeds) alone gives `live_minters: []`. A public-mint genesis with `PublicMint` outputs needs the trace: its live
minters are the live `PublicMint` cells with `remaining` above 0, so such a token carries `live_mint_authority` until minting is closed
(`live_minters: []`; its supply is then final at `supply + mint_allowance`). KaspaCom's program keeps minter lanes behind the extension
commitment, which the check cannot see (read the minter extension by hand).

Reproduce (both steps read-only; nothing is signed or submitted):

```
cd web && npm ci && npm run fetch-sdk && cd ..
node web/scripts/registry-genesis-evidence.mjs            # node (SDK Resolver, mainnet) + api.kaspa.org -> registry/evidence/mainnet-genesis.json
cargo run -p kob-cli -- registry verify-genesis --network mainnet [--json]
```

Public nodes prune block bodies after about 30 hours, so the genesis transactions (weeks old) and the spends that revealed their redeem
scripts come from the explorer; the node supplies liveness (its UTXO index). Nothing from the explorer is trusted: the verifier recomputes
the genesis txid from the recorded fields (the explorer omits `sequence`, `lock_time` and `gas`; they are recorded as 0, which the txid
confirms), recomputes the covenant id over the group, and checks each redeem script against its P2SH hash. A genesis output that was never
spent has not revealed its script; the collector then tries the states the issuer publishes (the KRON API record: creator key, vesting
covenant, dev amount) and records the one that hashes to the output. The KRON API is only a hint for the genesis txid of a covenant id. The
verifier exits non-zero when the registry's `genesis_verified` / `genesis` facts differ from what the evidence proves.

Warnings in the data model are derived from facts by code, never written per token (`TokenWarning`; the executor returns them in
`GET /v1/tokens` `warnings` and on order views, and UIs must show them):

- `genesis_unverified`: the entry does not say `genesis_verified: true` (and every open-list token without an entry,
  `OPEN_TOKEN_WARNINGS`): not every genesis output was checked against the program.
- `live_mint_authority`: the entry's `genesis` record lists live mint-authority cells (`live_minters` non-empty): the token's supply
  can still grow. The shipped registry has no such token.

Template `risks` (free text about a PROGRAM) declare what a program allows that is not a capability (for example the missing hardening of
`kron-2433`, the SIGHASH_ALL assumption of KRON address-presence balances, and the anyone-can-spend UTXO owned by a token's own
covenant id).

## Adding a token entry

An entry records facts; it is not a listing and changes nothing about whether the token trades (that follows from its program alone).

1. Make sure its program is in `templates` (new program: `kob registry add-program`, then the onboarding steps and the reviewer
   checklist of [`docs/spec/token-programs.md`](../docs/spec/token-programs.md)).
2. Add the token with `verified: false`; set `verified: true` after checking the covenant id and template hash against a chain UTXO,
   and `genesis_verified` plus its `genesis` record after `kob registry verify-genesis` checked every output of its genesis group
   (`true` when clean, `false` when not; add the token to the evidence by rerunning the collector).
3. Rerun `scripts/build-deploy.sh mainnet` (the deployment record pins this file) and commit the record with the change.
4. Run `cargo test -p kob-protocol registry`, `cargo test -p kob-tests --test registry_tests` and
   `cargo run -p kob-cli -- registry verify-genesis --network mainnet`.
