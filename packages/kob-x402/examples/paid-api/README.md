# Example: a paid API on the Kaspa x402 `exact` binding

`server.ts` sells `GET /report` for 0.5 KAS (or N units of a KCC-20 token, or by swapping token A), `client.ts` pays for it
with a dev key. The facilitator is the `kob-executor x402` service (same operator as indexer and matcher; it holds no keys).

```
payer client  --GET-->  paywall (server.ts)  --POST /settle-->  facilitator (kob-executor x402)  --> TN10 node
              <--402--                       <-- settlement --
              --GET + PAYMENT-SIGNATURE-->   serves /report after the settlement shows YOUR offer
```

## One-time setup

```
cd tools/wallet-gate
npm install
npm run fetch-sdk     # OFFICIAL rusty-kaspa v2.1.0 wasm SDK -> tools/wallet-gate/vendor/kaspa-node (gitignored).
                      # The npm package "kaspa-wasm" is stale (0.13.0, no tx v1 / covenants): never depend on it.
npm run keygen        # DEV key + address into tools/wallet-gate/.env (gitignored); NODE_WS defaults to `KOB_TN10_WRPC`, else `ws://127.0.0.1:18210` (see below)

cd ../../packages/kob-x402
npm install
# once, from the repo root: scripts/build-wasm.sh  (kob-wasm node bindings in crates/kob-wasm/pkg-node)
```

Requires Node >= 22 (the examples run as TypeScript directly).

## Fund the dev key (TN10)

The payer needs a few KAS. Either the faucet (https://faucet-tn10.kaspanet.io, behind a Cloudflare challenge: use a browser and
paste the printed address) or self-mine with the CPU miner in `tools/wallet-gate/miner` (see its README):

```
cd tools/wallet-gate/miner && cargo build --release --target-dir <outside-repo>
WALLET=<dev address> MAX_BLOCKS=100 <target-dir>/release/tn10-miner
<target-dir>/release/tn10-miner balance <dev address>
```

Coinbase maturity is 1000 DAA (about 100 s): mined funds are spendable after that.

## Run against the local facilitator on TN10

1. Start the facilitator against a TN10 node: a testnet-10 node of your own (`KOB_TN10_WRPC`, default `ws://127.0.0.1:18210`). See `kob-executor x402 --help` for the current
   flags; it needs the node wRPC URL, the network, the API key(s) it accepts and the token allowlist.
2. Start the paid API (merchant address = any TN10 Schnorr address you control, e.g. a second `keygen` key):

   ```
   KOB_X402_FACILITATOR=http://127.0.0.1:8402 \
   KOB_X402_API_KEY=devkey \
   KOB_X402_PAY_TO=kaspatest:qq... \
   node examples/paid-api/server.ts
   ```

   Optional offers: `KOB_X402_TOKEN=<covenant id> KOB_X402_TOKEN_UNITS=700` (KCC-20, with
   `KOB_X402_TOKEN_CUSTODY=issuer-controlled` to flag a token whose issuer keeps freeze/seize powers) and
   `KOB_X402_SWAP_PAY_ASSET=<covenant id of token A>` (swap-and-pay: pay with A, the merchant receives KAS).
3. Pay:

   ```
   KOB_X402_URL=http://127.0.0.1:8080/report node examples/paid-api/client.ts
   ```

   Without a payment the same URL answers `402` with a `PAYMENT-REQUIRED` header:
   `curl -i http://127.0.0.1:8080/report`.

## What the SDK guarantees (and where the example leans on it)

* The payer derives the request fingerprint itself (`sha256(canonical({method, url, body|null, paymentRequirementsHash}))`, the
  reference SDK's formula); the paywall recomputes it from the request it received and refuses a payload that carries another one.
* The payer rejects redirects on both requests and requires the effective URL to equal the requested URL.
* The signed artifact is written to `KOB_X402_ARTIFACT_DIR` **before** it is sent. If the process dies mid-payment,
  `client.resume(paymentId, init)` re-sends the stored artifact (the merchant is idempotent on payment id + request hash + transaction: only the very
  transaction a payment id was settled with is answered from memory, any other one under that id gets `409`
  `kaspa_payment_identifier_conflict`) and
  `client.revoke(paymentId)` invalidates it by spending one of its inputs back to the payer.
* A payment that fails is retried inside the same `paidFetch` (`retry`, default 3 attempts and 4 re-sends each, exponential
  backoff with jitter, never past the offer's `maxTimeoutSeconds`; [docs/spec/x402-retry.md](../../../../docs/spec/x402-retry.md)):
  an unknown outcome (no answer, a timeout, a 5xx, `settlement_pending`) RE-SENDS the same stored artifact, which the paywall
  and the facilitator answer from memory (a payment that went through is found, never paid again); a failure that left the
  payer's funds where they were (`order_conflict`: an order was taken by someone else; the node refused the transaction; the
  authorization expired) REBUILDS the payment from a fresh quote, under the same limits, `approve` asked again at the new cost,
  a fresh payment id, and spending the payment's anchor input (an input of the first attempt), so two attempts of one payment
  can never both be accepted; a refusal (policy, limits, an invalid offer, a payment id conflict) stops. `retry: false` sends
  once. The earlier attempts of a payment that settled end `superseded` in the store; when every attempt failed the error
  lists them (`e.attempts`).
* Calling `fetch` again does NOT silently sign a second, independent payment: while an earlier artifact for the resource is
  live the client refuses (`payment_in_flight`) unless the caller opts in with `allowResign`, and then it revokes the earlier
  artifacts (needs `submit`) before it signs anew. Paid requests for one resource run one at a time in a client.
* Nothing is paid without a spend authorisation: `capabilities.maxAmount` must name a ceiling for the merchant asset (and
  `maxPay` / `KOB_X402_MAX_PAY` bounds a swap per pay asset, e.g. `KAS=500000000,<token id>=900`; a bare number bounds KAS
  only; a swap is never built without one), or the `approve` hook must say yes to the built payment's cost
  (`spend_not_authorized` otherwise). The
  KAS a payer funds into a merchant token output (the carrier) is capped at 2 KAS by default (`maxCarrierSompi`). The carrier
  and the network fee count against the payer's KAS ceiling (`maxPay.KAS`, else `capabilities.maxAmount.KAS`) together with
  any KAS amount, and `approve` sees them (`cost.carrierSompi`, `cost.feeSompi`, `cost.kasSpent`).
* Custody (`unconditional` / `issuer-controlled`) is derived from the token program's registry capabilities, never from the offer.
* `PAYMENT-RESPONSE` must show success, the transaction id the payer itself signed and the accepted amount, or the payment is
  treated as pending and the response is not returned.
* The paywall serves the resource only after the facilitator's settlement shows the paywall's own offer amount and network.

## KCC-20 and swap-and-pay from the example client

kcc20 and swap-and-pay payments need the payer's token UTXOs and, for a swap, a quote of KOB orders. Those are covenant-owned
outputs and cannot be found by address, so the SDK takes them from an injected source. `client.ts` loads one from the module named
by `KOB_X402_SOURCES`:

```js
export const holdings = { '<covenant id>': '<base units>' };                       // what the payer holds
export async function tokens({ payerAddress, asset }) { return [/* kob-protocol TokenUtxo JSON */]; }
export async function quote({ payAsset, asset, amount }) { return { lockTime: '<recent DAA>', orders: [/* OrderRef JSON */] }; }
```

With it the client accepts kcc20 offers (when it holds enough) and swap-and-pay offers (`quote` is only called for swaps); the
default preference is standard-native, then kcc20, then swap routes. Without it the client is KAS-only (a route that accepts KAS as
its pay asset is still open to it). The executor's indexer API is the intended source.

Wallet mode: pass `wallet: { publicKey, signInputs(requests) }` instead of `privateKeys` and kcc20 / swap payments are built
unsigned, the wallet signs `built.sign` and only signatures come back (`x402BuildKcc20Unsigned` / `x402FinishKcc20`,
`x402PrepareSwap` / `x402FinishSwap` in kob-wasm). A native KAS payment signs the request authorization digest with the funding key, which
wallets do not offer, so native offers need a local key (and `revoke` does too).

`PAYMENT-SIGNATURE` for a token spend is tens of KB of base64, so the node server raises `maxHeaderSize` (`createNodeServer`, or the
`createServer` options in `server.ts`); put the same limit on any reverse proxy in front of it.
