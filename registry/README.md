# Token registry

Data behind KOB's token allowlist. Validated by `kob_protocol::registry` (`Registry::parse` / `validate`) against the structural schema
`tokens.schema.json` plus cross-field rules the schema cannot express. Nothing here is on-chain: listing is a KOB/operator decision
and is not enforced by the order contracts (an order pins its own covenant id and template hash).

| File | What |
|---|---|
| `tokens.json` | The registry shipped with a build (mainnet). It pins the six token programs of the strict list (reference KCC-20 3/3 and 8/8, the reference's published public-mint build `kcc20-ref-public-mint`, the two KRON programs, KaspaCom's KCC20 0.2.5) and lists the launch candidates: the KRON family of the 2026-09-29 census (template A `2ed46a7e...`: KRON, KASCOV, PEPE, ANSEM, IFWEN, PEEPS, DNBT; template B `8097c96f...`: KDIST). The two KRON templates are `reviewed` (internal review B2, 2026-09-30, with the conditions recorded in their `risks`: a per-token genesis check and no live minter before a token is listed or official); the KCC-20 templates are `pending-review`, KaspaCom's with finding K-1 confirmed by an engine PoC (the creator of a mint_policy 2 token can mint past max_supply or plant a non-program covenant UTXO through `set_public_mint_active`; see its `risks`). Listing verification (2026-10-03): all eight KRON tokens passed the genesis check on mainnet data (C1) and have no live mint authority (C2), so they are `listed`, each with its `genesis` record (re-derived from `evidence/mainnet-genesis.json` by `kob registry verify-genesis`). Six are `official`; PEPE ("The Ultimate test") and DNBT ("dont buy this is test") are test tokens per their own names and are NOT official (founder decision 2026-10-03): verified and listed, with a maintainer `warning` (PEPE's also says it is not the well-known PEPE, whose ticker it shares). The file is the release's pinned registry: its sha256 is in `contracts/deploy/mainnet/deployment.json` (`scripts/build-deploy.sh mainnet`, `docs/ops/release.md`). |
| `evidence/mainnet-genesis.json` | Chain evidence behind the `genesis` records: each token's genesis transaction, the redeem scripts of its genesis outputs and the node's liveness view, collected read-only by `web/scripts/registry-genesis-evidence.mjs`. Untrusted input: the verifier checks every byte against a hash. |
| `tokens.schema.json` | JSON Schema (draft 2020-12): structure, types, enums, hex patterns, `additionalProperties: false`. |
| `tokens.example.json` | Fictional TN10 entries (one `kcc20`, one `kron`; covenant ids are patterns, not deployed tokens). Format example and test fixture; unlike `tokens.json`, its templates are marked `reviewed` so the listing paths can be exercised. |

## Two lists: strict templates, open tokens

* **`templates` is the STRICT list.** Only token programs marked `review_status: reviewed` are on it. In the shipped `tokens.json` only the two KRON programs have reached that state, after an internal review with conditions (no independent audit has been done; the KCC-20 programs are `pending-review`). Because the token list is open (below), every token on a reviewed program is shown and tradable as `unverified`, with the `genesis_unverified` warning, until its entry is verified and listed. A program is accepted
  because its template hash is here, never by code special-casing: a third-party KCC-20-compatible program (same 112-byte state,
  draft transfer tags, owner scheme 4) such as KaspaCom's KCC20 0.2.5 is one more template entry plus its pinned artifact. Adding one
  needs the artifact under `contracts/third-party/` (with provenance and licence), a `TemplateId` in `kob-protocol`, its compute budgets
  and keeper tips (`KOB_REGEN=1`) and the engine tests.
* **`tokens` is the OPEN list.** Every token whose program is on the strict list is tradable and shown by the executor, whether or not it
  has an entry here (an order pins its own covenant id and template hash; the executor lists it when the hash is on the strict list).
  An entry adds metadata (ticker, decimals) and standing: `official` (confirmed genuine: shown with an
  "official" badge) or, by default, `unverified`. Tokens are identified by covenant id plus template hash: tickers collide (a KRON-launchpad
  "NACHO" is unrelated to the KRC-20 NACHO), so UIs always show the short covenant id next to the ticker.
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
- `max_token_inputs`, `max_token_outputs`: slot limits of the program (kcc20 reference 3/3, KOB 8/8 variant 8/8, KRON 4/5)
- `escrow`: owner types the order contracts use. kcc20: `owner_scheme` 4 (covenant id) and `borrow_scheme` 0. kron: `id_type` 2,
  `is_minter` 0 and `delivery_id_type` 3 (address presence, KRON-wallet compatible; 0 = pubkey is the alternative)
- `review_status`: `pending-review` | `reviewed`; `source`: path of the pinned artifact/bytes, upstream provenance

**`tokens`**:

- `ticker` (2-12 ASCII `A-Z0-9`), `name`, `family`, `covenant_id` (hex32), `template_id` (must exist and match the family)
- `extension_commitment` (hex32, required for kcc20, `null` for kron) and `extension_class` (`none` | `fixed-supply-standard` | `other`;
  `none` means an all-zero commitment on kcc20, and is the only class for kron)
- `decimals` (<= 18; an order's `scale` is `10^min(decimals, 9)`). Protocol v3 orders have no lot and no price tick: the legacy
  fields `lot_size` and `tick` of older files are read and ignored (this file no longer carries them)
- optional `max_token_inputs` / `max_token_outputs` (must equal the template's), `status` (`pending-review` | `listed` | `delisted`),
  `verified` (KOB checked covenant id and template hash against chain), optional `genesis_verified` (genesis check, see below: `true` =
  every genesis output checked and clean, `false` = checked and NOT clean, absent = not checked), optional `genesis` (the record behind it,
  see below), optional `warning` (free text UIs show next to the token, 1-512 characters), `official` (optional, default false: confirmed
  genuine; needs `verified`, `genesis_verified: true`, a `genesis` record with `live_minters: []` and `status: listed`), optional `display` (`description`, `website` https, `icon` https/ipfs,
  `kcc23` metadata object; all off-chain, KOB depends on none of it)

## Rules the validator enforces

- Identity is `(family, covenant_id, template hash, extension_commitment)` (`Registry::allowlist_key`); identities are unique, and so is each
  covenant id (one covenant id = one token). Asset identity for order books is the same tuple (fungibility only among equal
  extension commitments, as discussed in the KCC-20 thread on kas-smiths.org).
- Unknown fields anywhere are an error; hex is exactly 64 lowercase characters; unknown network / template / family mismatch are errors.
- `listed` requires `verified: true` and a `reviewed` template. New KRON token versions
  need a semantic review first. `official` requires `verified`, `genesis_verified: true`, a `genesis` record that found no live mint
  authority (`live_minters: []`) and `listed`; `genesis_verified: true` requires `verified`; a `genesis` record requires `genesis_verified`;
  a record with a live mint authority requires a `warning`.
- Tickers are ASCII uppercase alphanumerics. Two tickers that are equal after homoglyph normalisation (`O`->`0`, `I`/`L`->`1`) are refused
  (`KRON` vs `KR0N`). Names may not contain control, zero-width or bidi characters.

## Display

`display_name(token)` gives `TICKER (abcd…1234) [verified]`: ticker, first and last four hex digits of the covenant id, and the state
(`[official]`, `[verified]`, `[unverified]`, `[delisted]`). UIs show this string (never the name alone) so lookalike tokens are told apart by covenant id.

**Lookalikes and ticker collisions** (web `lookalikeReport`). A token outside the registry whose ticker or name equals or resembles a registered
token's is a strong warning ("possible impersonation: do not trade") when any token it resembles is official or carries no maintainer warning.
When every registered namesake is NOT official AND carries a `warning` (the registry itself disowns it, like the test tokens PEPE and DNBT), the
match is reported as a ticker collision instead (level `shared`: neither token is confirmed genuine; identify by covenant id), so a well-known
token is not called an impersonation of a test token. The validator still refuses confusable tickers WITHIN the registry, so listing another
"PEPE" later means renaming or removing this entry first.

## Relation to the launch allowlist

The allowlist row is `(covenant id, token template hash, suffix length, extension_commitment, decimals)` plus the family and slot
limits: `Registry::listed_allowlist()` returns exactly that for every `listed` token. `kob-executor --tokens registry/tokens.json` reads this file directly (validated, `listed` tokens only). Protocol v3 has no lot and no tick (orders are any amount in base units, prices per whole token). Operators may override the default list; the default is
maintained by pull request to `tokens.json`.

## Genesis check

A KIP-20 covenant id commits to the genesis outpoint and to the index, value and script public key of every output of the genesis group,
and nothing else: for P2SH outputs only to the hash of the redeem script. A token program reads balances and authority from any input
carrying its covenant id, so one hidden non-template output in a token's genesis (a "backdoor" script) can later mint look-alike tokens or
feed a fake balance into a genuine transfer, and neither the template nor a holder UTXO shows it. `kob_protocol::registry::verify_genesis`
recomputes the covenant id from the authorising outpoint and the outputs given (so they must be the complete group) and requires every
output to be the P2SH of a revealed redeem script that is an instance of the token's pinned program with a decodable state; it returns
the output count, the supply and the outputs that carry a mint authority. Record the result as `genesis_verified: true` (clean) or `false`
(plus a `warning`), and the facts as the token's `genesis` record:

```
genesis: { txid, daa_score, outputs: [..], supply, minter_outputs: [..], live_minters: ["txid:index", ..] (optional),
           checked_at_daa, source }
```

`live_minters` is condition C2 of the KRON review: `[]` = no live mint authority, absent = not determined. For the KRON programs it
follows from the genesis: only a transaction that spends a minter can create an `is_minter` output (every non-minter token input refuses
one, `review_b2_kron` r_kr_01 / r_kr_15), and a covenant id's only other source of outputs is its genesis, so a genesis without a minter
output proves the token never has a live minter. A genesis with one needs the minter's lineage traced to its live cells. KCC-20 programs
keep minter lanes behind the extension commitment, which the check cannot see (KaspaCom: read the minter extension by hand).

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

Warnings in the data model: a token whose entry does not say `genesis_verified: true`, and every open-list token without an entry, carries
the warning code `genesis_unverified` (`TokenWarning`, `OPEN_TOKEN_WARNINGS`); the executor returns it in `GET /v1/tokens` `warnings`, and
UIs must show it. Template `risks` (free text) declare what a program allows that is not a capability (for example the missing hardening of
`kron-2433`, the SIGHASH_ALL assumption of KRON address-presence balances, and the anyone-can-spend UTXO owned by a token's own
covenant id).

## Adding a token

1. Make sure its program is in `templates` (new program: pin the artifact or bytes, add the entry, extend `registry_tests.rs`, do the review).
2. Add the token with `status: "pending-review"`, `verified: false`; set `verified: true` after checking the covenant id and template hash against a chain UTXO,
   and `genesis_verified: true` plus its `genesis` record after `kob registry verify-genesis` accepted every output of its genesis group
   (`false` and a `warning` when it did not; add the token to the evidence by rerunning the collector).
3. After review set `status: "listed"`; `official: true` only with `live_minters: []` (and only for a token that is
   meant to be genuine: a test token stays non-official with a `warning`).
4. Rerun `scripts/build-deploy.sh mainnet` (the deployment record pins this file) and commit the record with the change.
5. Run `cargo test -p kob-protocol registry`, `cargo test -p kob-tests --test registry_tests` and
   `cargo run -p kob-cli -- registry verify-genesis --network mainnet`.
