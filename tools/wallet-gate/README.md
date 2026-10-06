# KOB wallet gate kit

**Question:** can the browser wallets **KasWare**, **Kaspire** (and **Kastle**, for reference) sign a **covenant (P2SH) input** in a
Toccata tx v1 on **testnet-10**? KOB asks, cancels and KCC-20 payments all need the user's key to sign such an input, so this is the
go/no-go gate for the web app's wallet support.

Three tests, each builds an unsigned v1 tx, asks the wallet to sign **only input 0**, extracts the signature, assembles the sigscript
itself (`pushed args + 4-byte dispatch tag + redeem script`), broadcasts through a TN10 node and waits for acceptance:

| Test | Input | What is signed |
|---|---|---|
| **T1** baseline | wallet's own P2PK UTXO | plain P2PK spend of a v1 tx with `computeBudget` set |
| **T2** | `BidOrder` covenant UTXO whose maker key = wallet key | `BidOrder.cancel(sig)` (covenant input, sighash ALL) |
| **T3** | KCC-20 token UTXO owned by the wallet key (owner scheme `0x00`, borrow disabled) | `transfer(next_states, witness = 0x00 || sig)` to a new owner |

## What is verified without any wallet (already run, TN10, 2026-09-29)

* `node test/selfcheck.mjs` - offline: push encoders equal `kaspa-wasm`'s `ScriptBuilder`; the KCC-20 state encoder reproduces the compiled
  artifact byte-for-byte; BidOrder template patching; sigscript layout and signature placement (T2 sigscript = 895 B, same as the Rust harness);
  a Kaspire-style `ordered-args` assembly equals ours.
* `node test/e2e-devkey.mjs` - **live on TN10** with a local key in place of the wallet: setup + T1 + T2 + T3 all accepted by the node
  (results in `out/e2e-devkey.json`).
* `node test/page-mock.mjs` - drives the real page in headless Chrome against TN10 with a mock wallet: T1-T3 accepted.

So a failure with a real wallet is a wallet-side result, not a tx-shape problem in the kit.

## One-time install

Requires Node >= 22.

```
cd tools/wallet-gate
npm install
npm run fetch-sdk          # official rusty-kaspa v2.1.0 wasm SDK -> vendor/ (the npm "kaspa-wasm" package is stale 0.13.0, do NOT use it)
npm run keygen             # writes a DEV key + address into the gitignored .env; NODE_WS defaults to `KOB_TN10_WRPC`, else ws://127.0.0.1:18210 (a testnet-10 node of your own)
node test/selfcheck.mjs    # offline sanity
```

`NODE_WS` may be left empty in `.env` to use kaspa-wasm's public `Resolver` instead of your own node.

## Fund the dev key (TN10 coins)

The setup transaction is funded by the dev key. It needs roughly 45 KAS per wallet under test.

* **Faucet:** https://faucet-tn10.kaspanet.io (sits behind a Cloudflare challenge: use a normal browser, paste the printed `DEV address`).
  Repeat if the amount per request is small.
* **Self-mine (no faucet):** `miner/README.md` - a CPU miner (Rust, rusty-kaspa v2.1.0) that mines straight to any address on your own TN10 node,
  ~2 s per block on this machine, ~3 KAS per block, spendable after ~100 s (coinbase maturity):
  `cd miner && cargo build --release && WALLET=<dev address> MAX_BLOCKS=100 target/release/tn10-miner`.
  Check the balance with `tn10-miner balance <address>`.

## Install the wallets and switch to testnet-10

Use throw-away test accounts only. Never import a mainnet mnemonic.

| Wallet | Install | Switch network |
|---|---|---|
| KasWare | Chrome Web Store, "KasWare Wallet" (id `hklhheigdmpoolooomdihmhlpjjdbklf`) | wallet header network selector -> **Testnet 10** |
| Kaspire | https://github.com/KaspaHUB21/Kaspire-Kaspa-Wallet (Chrome extension release; Android app 0.11.x also exists but the page needs a desktop extension) | network selector -> **testnet-10** |
| Kastle | Chrome Web Store, "Kastle" (Forbole) | settings -> network -> **testnet-10** |

Create/import an account whose address starts with `kaspatest:q` (Schnorr P2PK). Copy that address for each wallet.

## Run

1. **Setup, once per wallet account** (creates, for that account's key, on TN10: 4 fixed-supply KCC-20 token UTXOs (reference template,
   owner scheme 0x00, borrow disabled, 0x04 enabled in the template config), 4 BidOrder UTXOs with maker = the wallet key (3 KAS each),
   3 plain 5 KAS UTXOs for T1):

   ```
   node scripts/setup.mjs --wallet-address kaspatest:q...  --label kasware
   ```

   It prints the tx id and writes `state/setup-<pubkey>.json`. Each test consumes one item; rerun setup for more (dev key pays).
2. **Start the page:** `npm run serve`, open **http://localhost:8787/** in the browser profile that has the wallet extension
   (http://localhost is a secure context, so extensions inject their provider).
3. **Connect** the wallet (button in section 1). The page shows the wallet's version, network, address and public key, and loads the
   matching setup file. The network must read `testnet-10`.
4. Click **T1**, approve the popup. Then **T2**, then **T3**. For each run, before or after approving, fill the "popup appeared / what it
   displayed" fields (or paste into `RESULTS.md`). Kastle: also try the three "Kastle variant" options for T2/T3 (plain / empty-script / redeem-script);
   Kaspire: default is `ordered-args`, also try `wrap-signature` if that fails.
5. **Download results JSON** (button) and copy the table into `RESULTS.md`. Every run is also appended to `out/page-results.jsonl`.

The log and the result row show: signature returned (yes/no), whether the wallet's own signature script equals ours (Kaspire returns a full
sigscript; a match independently validates our ABI encoding), `walletKeptFields` (did the wallet keep version 1, computeBudget and outputs?),
the node's rejection text if any, and acceptance (the output appears in the node's UTXO set).

### Reading failures

| Symptom | Meaning |
|---|---|
| `wallet signing failed: ...` | wallet refused or threw (text is recorded verbatim) |
| `no signature in wallet response` | wallet returned the tx with an empty `signatureScript` on input 0 (Kastle issue #353 behaviour) |
| `node rejected the tx: ... signature` / `script` error | wallet signed a different digest (e.g. it altered the tx or used another sighash) |
| `not seen in UTXO set within 90s` | node accepted into mempool but not in a block yet; wait and check the txid |

## Automated wallet runs (no manual clicking)

`test/wallets/kasware.mjs`, `kaspire.mjs`, `kastle.mjs` drive the real released extensions with Playwright's bundled Chromium (branded Chrome >= 137
ignores `--load-extension`; run `npx playwright-core install chromium` once), import a random test mnemonic, switch to TN10, run setup, and click through the
wallet approval popups. Results: `out/wallet-<name>.json`, screenshots in `out/screens/<name>/`. Summary in `RESULTS.md` (section "Automated run").

## Layout

```
lib/script.mjs      pure-JS script pushes + SilverScript ABI sigscript / state encoder (node + browser)
lib/contracts.mjs   KCC-20 state/redeem, BidOrder template patching (maker, tokenCovId), sigscript builders
lib/txbuild.mjs     tx v1 plan -> wasm Transaction, exact fee floor (100 sompi * max(compute mass, 2*size)), RPC helpers
lib/flows.mjs       T1/T2/T3 builders, local signing, sigscript assembly, signature extraction, Kaspire ordered-args templates
lib/setup-core.mjs  one genesis tx: token covenant group + bids (own covenant ids) + wallet P2PK funds
scripts/            fetch-sdk, keygen, setup, serve, build-templates (dev-only; needs silverc)
web/                the static page (index.html, app.mjs, wallets.mjs adapters)
artifacts/          KCC20Ref.json (silverc v1.0.0), BidOrder.template.json (sentinel-compiled; maker/tokenCovId patched at runtime)
contracts/          the .sil sources
test/               selfcheck, e2e-devkey, page-mock, wallets/*
miner/              TN10 CPU miner
```

## Wallet API facts and uncertainties (as of 2026-09-29)

* **KasWare:** `window.kasware.signPskt({txJsonString, options:{signInputs:[{index, sighashType}]}})`, returns Safe-JSON tx; v1/covenant handling lives in a
  private wasm build, undocumented. Provider network label `testnet-10`, `getPublicKey` may return a 33-byte key (the page normalises to x-only).
* **Kaspire:** `window.kaspire.request({method:'signPskt', params:{psktTransactionJson, signInputs, scripts:[{inputIndex, scriptHex, signType, signatureScript:{mode:'ordered-args', args}}]}})`;
  arg types `signature{prefixHex}`, `i64`, `data{hex}`, `byte`. It appends the redeem script and verifies its hash against the input's UTXO script.
  Uncertain whether its `data` push canonicalises 1-byte values exactly like the SilverScript encoder (checked by `walletSigscriptEqualsOurs`).
* **Kastle:** `kastle.signTx(networkId, txJson, scripts?)`; issue #353: P2SH inputs are silently left unsigned without `scripts`; the `scriptHex:""` workaround is untested upstream.
* **KIP-12** (`kaspa:requestProvider` / `kaspa:provider`) is a draft; the page lists announced providers but only the three adapters above are exercised.
* The page cannot verify a returned Schnorr signature itself (the v1 sighash needs the full consensus implementation); validity == the node accepted the tx.
* Hardware wallets are out of scope (cannot sign covenant txs today).

## Security notes

Everything here is TN10 test money. Secrets (dev key, simulated wallet key, test mnemonics) live only in the gitignored `.env`; `state/`, `out/`,
`vendor/` and `.browser-profiles/` are gitignored too. The static server binds to 127.0.0.1 and serves only `web/ lib/ artifacts/ state/ vendor/kaspa-web/`.
