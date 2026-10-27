# Token metadata on chain (KCC-23)

Status: adopted 2026-10-19. A token KOB creates declares its name, ticker, decimals and links on chain under KCC-23 "MJ: Metadata
JSON", and KOB reads a token's facts from that declaration before anything else. KOB holds no authoritative token-metadata
database: the facts come from the chain, and the registry (`registry/tokens.json`) only keeps a pinned snapshot of them, checked
against the chain. Curation (an official status, anti-impersonation, logos) is not KOB's: it belongs to external community token
lists the user chooses (`registry/README.md`, "Curation is not KOB's").

## 1. The standard and its pin

| Item | Value |
|---|---|
| Spec | KCC-23 "MJ: Metadata JSON", kaspanet/kccs `kcc-0023.md` |
| Upstream change | `main` `1516a6c4cef59940fc3af3096dbcdd9c08690254` ("Add KCC-23: MJ (Metadata JSON) (#26)", merged 2026-10-14) |
| Upstream status | **Draft**: the format may still change |
| Conformance vectors | `crates/kob-tests/vectors/kcc23/conformance.json` (unmodified, sha256 pinned; see `PROVENANCE.md` there), run by `cargo test -p kob-tests --test kcc23_conformance_tests`; every vector passes against KOB's rusty-kaspa pin (v2.1.0) |
| Implementation | `crates/kob-protocol/src/kcc23.rs`, the one module that knows the format (codec, binding check, launch proof, KOB's field convention). The web app, the CLI and the executor reach it through kob-wasm and kob-protocol, so a change of the Draft is an edit there plus the vectors |

What KCC-23 itself specifies (section 3 of the spec; KOB follows it as written):

* **Declaration.** The payload of a **version 1** *preparation transaction* is one codec byte followed by the encoded value. The
  codec `0x00` is a UTF-8 JSON object; no other codec is defined and a reader refuses any other byte. The object must be
  I-JSON (RFC 7493: valid UTF-8, no unpaired surrogates, no duplicate member names), carry no byte-order mark, have numbers that
  convert to finite IEEE-754 binary64 values, and nest at most 64 levels (the root object is depth 1). The commitment covers the
  exact bytes: whitespace and member order included.
* **Deployment.** Two transactions: the preparation, creating an output, then the genesis, spending that output to authorise the
  KIP-20 genesis output group. The preparation's version 1 id commits to the payload; the covenant id commits to the spent
  outpoint and the group; together they bind the declaration to the covenant id.
* **Verification** (optional): the preparation is version 1 and its payload valid; its id recomputed; a genesis input spends an
  existing output of it; the covenant id recomputed from that outpoint and the group the input authorises equals the expected one.
  This establishes the binding, not acceptance on chain.
* **Field names and meanings are left to ecosystem conventions.** The example of the spec uses `name`, `symbol` and `decimals`.

## 2. KOB's field convention

The fields below are **KOB's convention**, not part of KCC-23. The first three are the ones the KCC-23 example uses; the rest are
KOB's choice (kept short: the payload is paid for in mass). KOB writes them in this order, compact (no whitespace), leaving out
the optional ones that are not set:

| Field | Type | KOB writes | A KOB reader takes |
|---|---|---|---|
| `name` | string | 1..=64 characters, no control, zero-width or bidi character | the same rule |
| `symbol` | string | 2..=12 of `A-Z 0-9` (KOB's ticker rule) | 1..=32 characters, no bad character, no white space |
| `decimals` | integer | 0..=18 | a JSON integer 0..=18 (`8.0`, `"8"`, `19`, `-1` are not taken) |
| `description` | string, optional | 1..=512 characters, no bad character (line breaks are refused) | the same rule |
| `logo` | string, optional | an `https://` or `ipfs://` address, at most 512 characters | the same rule |
| `website` | string, optional | an `https://` address, at most 512 characters | the same rule |
| `x` | string, optional | `https://x.com/<handle>`, handle 1..=15 of `A-Z a-z 0-9 _` (the CLI and the app turn `@handle` into this) | the same rule |
| `telegram` | string, optional | `https://t.me/<handle>`, handle 5..=32 of `A-Z a-z 0-9 _` | the same rule |

* A reader takes each field on its own: a field of the wrong type or out of range is dropped (and named in the reader's
  `issues`), unknown fields are ignored, and a declaration without a usable `decimals` leaves the token's decimals undeclared.
* Facts the chain states by itself are not declared again: the supply is the sum of the genesis outputs, the program its template
  hash. (`totalSupply`, `fixedSupply` and `standard` of KOB's older off-chain `metadata.json` are gone; its `icon` is now `logo`.)
* The token's facts in the registry format that issuance writes (`registry_entry.json`, for tools and community lists that read that
  format; not a listing request) keep `display.icon` / `display.x` / `display.telegram` and carry the
  declared object verbatim as `display.kcc23`.

### Size limits

| Limit | Value | Where |
|---|---|---|
| Payload KOB writes (codec byte included) | **1024 bytes** (`kcc23::MAX_TOKEN_PAYLOAD`) | refused before planning (the form says how many bytes the details take: `metadata.too_large`); name, symbol and decimals alone are about 50 bytes |
| Payload a KOB reader decodes | 16 KiB (`kcc23::MAX_READ_PAYLOAD`) | a reader policy, not a KCC-23 rule: larger payloads are left undecoded |
| A candidate preparation the executor keeps | 32 KiB encoded | `indexer::kcc23::MAX_CANDIDATE_BYTES` |

### Example

`{"name":"Example Token","symbol":"EXT","decimals":8}` (the KCC-23 example, which KOB's writer produces byte for byte):

```text
007b226e616d65223a224578616d706c6520546f6b656e222c2273796d626f6c223a22455854222c22646563696d616c73223a387d
```

## 3. Creating a token

`kob_protocol::issue::build_creation` (CLI `kob token issue`, the web app's Create token page through kob-wasm `issue`):

| Transaction | Inputs | Outputs | Payload |
|---|---|---|---|
| 1. preparation (version 1) | the creator's P2PK funding | 0: P2PK of the creator, exactly the genesis's carriers plus its fee; 1: change (dropped into the fee below the dust bound) | `0x00` + the declaration |
| 2. genesis (version 1) | the preparation's output 0, alone (input 0 authorises the KIP-20 genesis group) | the token outputs, no change | the states of the token outputs (`KOBG`, below) |

**The genesis payload: why the issued outputs name their states.** A token output is a P2SH output: its script hash commits to the
whole state (amount, owner, ...), so the state cannot be read from the chain. A transfer reveals the states it creates (the leader's
`next_states`), but a genesis has no such leader, so until the first spend of an issued output nothing but the issuing wallet's own
storage knew its state: a token created in one browser could not be listed or sold from another browser, or by the CLI, of the same
key. The genesis therefore carries, in its payload, the states of its token outputs (`kob_protocol::genesis_states`):

```text
"KOBG" | version 1 | count (1..=255) | count x ( length (1..=255) | state span )
```

* The spans are the exact bytes the token program commits to (the 112-byte KCC-20 state), in output order. A reader keeps a span only
  when the P2SH it produces under a pinned token program equals the output's script public key, exactly as for a transfer's states:
  the payload proves nothing by itself, a wrong or hostile one can only fail to be believed, and it cannot change the token (the
  covenant id commits to the authorising outpoint and the output group, not to the payload).
* The executor's indexer lists each proven output as a holding of its owner (`GET /v1/token-utxos?owner=`, role `owned`) from the block
  of the genesis; the web app and `kob` take their token UTXOs from that endpoint and confirm each on the node before spending it. A
  KCC-23 reader ignores the payload (it reads the preparation's).
* A genesis built before this payload existed, or by another tool, carries none; see *What the issuer must keep*.

* Both are planned and signed before anything is sent: a version 1 transaction id does not depend on signature scripts or compute
  budgets, so the genesis can spend an output of a preparation that is not on chain yet. The preparation is sent first, then the
  genesis (offered again for a few seconds while the node has not yet seen its input).
* Lock time 0, the native subnetwork and gas 0 on both (what the executor's indexer can hash from a `High` VSPC answer).
* Fees at the relay floor (100 sompi per gram), from `issue::tests::creation_fees_before_and_after`:

  | Creation | Before (one genesis, no payload) | KCC-23 (preparation + genesis, empty genesis payload) | With the genesis payload (this build) |
  |---|---|---|---|
  | one holder, name / symbol / decimals only (the app's default; 51-byte declaration) | 208,300 sompi | 208,900 + 167,100 = 376,000 sompi | 208,900 + 179,000 = 387,900 sompi |
  | two holders, every optional field (204-byte declaration) | 254,000 sompi | 224,200 + 212,800 = 437,000 sompi | 224,200 + 236,000 = 460,200 sompi |

  The genesis payload (113 bytes per token output) adds about 0.0001 KAS per output at this fee rate.
  The KAS locked in the token outputs (10 KAS each by default) is unchanged.

### Signing

* **Private key** (the sandboxed signer frame): ONE request, `sign {built: genesis, prep, genesis: <claimed token outputs>}`, on
  the frame's one confirmation. The frame checks, from the transactions alone, that the genesis spends exactly the preparation's
  output 0, that every preparation input is a coin of its key and every preparation output pays its key, that the payload is a
  KCC-23 declaration, and that each genesis token output carries exactly the script of its claimed state; it then shows the
  declared name, ticker and decimals, the supply and what the key receives, and one fee for both. Approve signs both
  (`prepSignatures` with the genesis's `signatures`).
* **Extension wallets** (KasWare, Kaspire, Kastle): their APIs sign one transaction per request, so the wallet asks twice (the
  preparation, then the genesis), after the app's one confirmation screen, which says so. Both are signed before anything is sent:
  declining the second request sends nothing.
* The app's confirmation screen verifies the preparation too (inputs and outputs the wallet's own, the declaration states the shown
  name, ticker and decimals, the fee is inputs minus outputs) and shows what is recorded on chain.

### Failure between the two

* The preparation's output 0 is an ordinary P2PK UTXO of the creator: if the genesis never goes out, the KAS stays in the wallet,
  spendable like any other coin. Nothing can be lost.
* The web app writes a creation record (`data/creations.ts`: both signed transactions, the token, the documents) before it sends the
  preparation, and removes it once the genesis is accepted. A record that outlives the page is offered on Create token: "Send the
  creation" sends the saved preparation again (a node that has it refuses it, harmlessly) and the saved, signed genesis, with no
  new signature; "Forget it" keeps the KAS. A genesis already on chain finishes its record silently.
* The CLI keeps the same in the output directory (`./kob-create-<id16>` when no `--out-dir` is given on a submitting run);
  `kob token resume <DIR>` sends the saved pair.

### What the issuer must keep

Whether the issued tokens can be found again depends on whether the genesis named their states (*The genesis payload* above).

* **A creation of this build or later** (the web app's Create token page, `kob token issue`): nothing. The genesis names the states
  of its outputs and an upgraded indexer lists them to the owner, so the same key, in any browser or on any machine, sees the
  tokens (the web app's balances and ticket, `kob place`, `kob send`, `kob merge`) from the block of the genesis. An indexer that was
  not running then needs a rebuild from an earlier start (`docs/ops/executor.md`, Part B 5.1). Keep the key; keep `supply.json`
  (the Create token result panel offers it; the CLI writes it to the output directory) as the record of what was issued where.
* **A creation of an earlier build, or a genesis made by another tool, whose payload is empty.** The state of each issued output was
  known to the issuing browser only (its token tracker, in local storage; the CLI's record keeps a day) until the output is first
  spent. The existing backups do **not** cover it unless made for this purpose: the placement-records backup (*Orders, Recover*) holds
  orders, not token outputs, and the tracker's export (*Settings, Token tracker, Export*) holds the outputs that browser tracked
  when it was made. The issuer keeps one of:
  1. the tracker export of the browser that created the token (import it in *Settings, Token tracker*; each entry is checked on the
     node before use); or
  2. the token's covenant id and the amount the creation gave the key (what `supply.json` lists as the allocation of the output, or
     what was typed in the form): *Settings, Token tracker, Find a token you created earlier* derives the state (the key, that amount,
     the standard fixed-supply program with no borrowing and extension commitment zero), asks the node for the output at its script and
     remembers it. It finds the output only for a state of that exact shape; an allocation to a custody owner, a borrow scheme, an
     extension commitment other than zero or an amount the issuer no longer knows has no such path. Once found, spending the
     output (a send to oneself will do) makes it known to every indexer, because a transfer names the states it creates.

## 4. Reading a token's facts

* **Launch proof** (`kob-kcc23-proof/1`, `kcc23::LaunchProof`): `{format, covenantId, preparation, genesis}`, each transaction in
  the fields its version 1 id commits to (`version`, `inputs` [`transactionId`, `index`, `sequence`], `outputs` [`value`,
  `scriptPublicKey` {`version`, `script`}, `covenant`], `lockTime`, `subnetworkId`, `gas`, `payload`; bytes as hex, 64-bit numbers
  as decimal strings). `LaunchProof::verify` (kob-wasm `kcc23Verify`, CLI `kob token verify-declaration`) recomputes both ids and
  the covenant id: whoever supplies a proof (the indexer, a list, a file) can withhold a declaration, never forge one. A proof is
  also how a token whose genesis predates an indexer is verified (`launch_proof.json` of the creation).
* **Indexer** (`docs/ops/executor.md`): the executor keeps every candidate preparation it sees accepted, and when a genesis spends
  one it stores the launch proof after verifying it; `GET /v1/tokens` marks declared tokens (`declaration: true`) and
  `GET /v1/tokens/{covenant_id}/declaration` serves the proof. Nodes keep no transaction index and prune, so the declaration is
  captured as the genesis is accepted; candidates no genesis spent within `declaration_window_hours` (72) are dropped.
* **Web app** (`data/token-declarations.ts`): proofs from the indexer (asked only for tokens `GET /v1/tokens` marks as declared) and
  the creator's own (at once) are verified in kob-wasm
  against the covenant id asked for, kept in memory and, as proofs, in localStorage (re-verified when read back). The facts a token
  is shown with, in order (`app/token-display.ts`):
  1. its verified declaration (marked "declared by its creator" for a token the registry does not list: the creator's own claim, not
     reviewed);
  2. without a declaration (a token created before KCC-23: its id cannot commit to one), as before: the registry entry, this
     browser's issuance record, the decimals of its market's price scale, else 8 assumed.
* **Names from a token list service (display only).** The market list of the web app is completed from a service such as kascov.io
  (`data/kascov.ts`, `docs/ops/web-hosting.md`): it supplies the creator's claimed ticker, name and image for tokens the steps above
  name nothing for, labelled "declared by its creator", and the tokens of a program this build trades that no order has named yet.
  Nothing it says is a fact: the ticker and decimals above win and a disagreement is a note, an image is shown only if its bytes
  hash to the hash committed in the token's creation record, and its ratings are not read. KOB keeps no list of its own.
* **CLI** (`kob place`, `send`, `merge`, the order actions): the indexer's proof, verified against the covenant id, the same read
  order and the same refusals; a declared token without orders is traded by its covenant id from a wallet holding it
  (`crates/kob-cli/README.md`).
* **Registry and chain disagree.** The registry is a pinned snapshot; a fact in it that differs from the declaration is never silently
  preferred: the on-chain values are shown, flagged "facts disagree" with the fields named (and a banner on the token's market),
  and when the **decimals** differ no order of the token is planned until the two agree (the same base units would otherwise mean
  other whole-token amounts and prices). The same holds for a market whose orders use another price scale than the declared
  decimals give (`10^min(decimals, 9)`): an open-list token's first order takes its scale from the declaration, and orders of
  another scale are a `scale-conflict`.

## 5. Open points

* KCC-23 is a Draft. Open upstream questions (the Kas Smiths thread, topic 150): strict JSON edge cases (handled here by the I-JSON
  rules the merged text states), whether every output of a preparation or only the one spent declares (KOB binds the one its
  genesis spends, as the reference verifier does), and the dependency on KIP-24 for the version 1 id.
* The executor lists a token in `GET /v1/tokens` once an order names it (a genesis output is a P2SH whose program it cannot see), so
  a declared token without any order is not marked there yet and other browsers show it as before until its first order. The proof
  is still served by `GET /v1/tokens/{id}/declaration`.
* Field names are a convention: other ecosystem tools may pick other names for the optional fields (`logo` vs `icon`). A reader
  ignores fields it does not know; KOB names only what it writes.
