// Example payer: fetches a paywalled URL with a dev key. Uses the OFFICIAL kaspa-wasm v2.1.0 SDK vendored by
// tools/wallet-gate (`npm run fetch-sdk` there), the node's wRPC for UTXOs, and kob-wasm for building and signing.
//
//   KOB_X402_URL=http://127.0.0.1:8080/report  node examples/paid-api/client.ts
//
// Environment:
//   KOB_X402_URL           the paid URL (default http://127.0.0.1:8080/report)
//   KOB_X402_DEV_KEY       32-byte hex secret key; default: DEV_PRIVATE_KEY from tools/wallet-gate/.env (`npm run keygen`)
//   KOB_X402_NODE_WS       node wRPC JSON url (falls back to KOB_TN10_WRPC, then ws://127.0.0.1:18210: a testnet-10 node of your own)
//   KOB_X402_NETWORK       kaspa:testnet-10 (default) | kaspa:mainnet
//   KOB_X402_SDK_DIR       kaspa-wasm nodejs build (default tools/wallet-gate/vendor/kaspa-node)
//   KOB_X402_ARTIFACT_DIR  where signed artifacts are recorded before they are sent (default ./.x402-artifacts)
//   KOB_X402_MAX_FEE       highest fee in sompi the payer accepts (default 5000000)
//   KOB_X402_SOURCES       module with the token / order sources (see below); default: KAS only
//
// KAS is the default. kcc20 and swap-and-pay payments additionally need the payer's token UTXOs and, for a swap, a quote
// of KOB orders to fill: covenant-owned outputs cannot be found by address, so they come from an injected source (the
// executor indexer, when its API is available). Point KOB_X402_SOURCES at a module that exports
//
//   export const holdings = { '<covenant id>': '<base units>' };                        // what the payer holds
//   export async function tokens({ payerAddress, asset }) { return [/* kob-protocol TokenUtxo JSON */]; }
//   export async function quote({ payAsset, asset, amount }) { return { lockTime: '...', orders: [/* OrderRef JSON */] }; }
//
// and the client will also accept kcc20 and swap-and-pay offers (`quote` only for swaps). Without it the client is
// KAS-only.

import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import {
  FileArtifactStore,
  KobX402Client,
  KobX402Error,
  connectRpc,
  deriveAddress,
  loadKaspaNodeSdk,
  loadKobWasm,
  rpcContextProvider,
  rpcSubmitter,
} from '../../src/index.ts';
import type { NetworkId, PayerCapabilities, SwapQuote, TokenUtxoJson } from '../../src/index.ts';

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, '../../../..');
const gate = join(repoRoot, 'tools/wallet-gate');

function devKey(): string {
  if (process.env.KOB_X402_DEV_KEY) return process.env.KOB_X402_DEV_KEY;
  const envFile = join(gate, '.env');
  if (existsSync(envFile)) {
    const m = /^\s*DEV_PRIVATE_KEY\s*=\s*"?([0-9a-fA-F]{64})"?\s*$/m.exec(readFileSync(envFile, 'utf8'));
    if (m?.[1]) return m[1];
  }
  console.error('no dev key: set KOB_X402_DEV_KEY or run `npm run keygen` in tools/wallet-gate');
  process.exit(2);
}

const network = (process.env.KOB_X402_NETWORK ?? 'kaspa:testnet-10') as NetworkId;
const url = process.env.KOB_X402_URL ?? 'http://127.0.0.1:8080/report';
const key = devKey();

interface Sources {
  holdings?: Record<string, string>;
  tokens?: (q: { payerAddress: string; asset: string }) => Promise<TokenUtxoJson[]>;
  quote?: (q: { payAsset: string; asset: string; amount: string }) => Promise<SwapQuote>;
}
const sources: Sources = process.env.KOB_X402_SOURCES ? ((await import(pathToFileURL(resolve(process.env.KOB_X402_SOURCES)).href)) as Sources) : {};
// Nothing is paid without an explicit ceiling per merchant asset: 1 KAS, and at most what the payer holds of a token.
const capabilities: Omit<PayerCapabilities, 'network'> = sources.tokens
  ? { tokens: sources.holdings ?? {}, maxAmount: { KAS: '100000000', ...(sources.holdings ?? {}) } }
  : { kasOnly: true, maxAmount: { KAS: '100000000' } };

const sdk = loadKaspaNodeSdk(process.env.KOB_X402_SDK_DIR ?? join(gate, 'vendor/kaspa-node'));
const rpc = await connectRpc(sdk, { url: process.env.KOB_X402_NODE_WS ?? process.env.KOB_TN10_WRPC ?? 'ws://127.0.0.1:18210', network });
const { address } = deriveAddress(sdk, key, network);
console.log('payer', address);

const client = new KobX402Client({
  wasm: await loadKobWasm(),
  network,
  payerAddress: address,
  privateKeys: [key],
  context: rpcContextProvider(rpc, { ...(sources.tokens ? { tokens: sources.tokens } : {}), ...(sources.quote ? { quote: sources.quote } : {}) }),
  store: new FileArtifactStore(process.env.KOB_X402_ARTIFACT_DIR ?? '.x402-artifacts'),
  submit: rpcSubmitter(sdk, rpc),
  capabilities,
  maxFeeSompi: process.env.KOB_X402_MAX_FEE ?? '5000000',
  // swap-and-pay needs an explicit bound on what it may cost, per pay asset and in that asset's units:
  // KOB_X402_MAX_PAY="KAS=500000000,<token covenant id>=900" (a bare number counts sompi of KAS only)
  ...(process.env.KOB_X402_MAX_PAY ? { maxPay: parseMaxPay(process.env.KOB_X402_MAX_PAY) } : {}),
  // The demo retries a retryable failure below. That signs a SECOND payment, so it is an explicit policy: the client first
  // revokes the earlier artifact (needs `submit`) and refuses to sign again when the revoke was not submitted.
  allowResign: () => true,
});

try {
  // The SDK never re-sends a payment on its own. A failure the facilitator marks `retryable` (an order that was
  // consumed meanwhile) is the CALLER's decision: calling again re-quotes and re-signs, after revoking the first artifact.
  for (let attempt = 1; ; attempt++) {
    try {
      const { response, payment } = await client.paidFetch(url);
      console.log(response.status, await response.text());
      if (payment) console.log('paid', payment.amount, payment.asset, 'tx', payment.transactionId);
      break;
    } catch (e) {
      if (e instanceof KobX402Error && e.retryable && attempt < 3) {
        console.warn(`retryable failure (${e.diagnostic}): ${e.message}; trying again with a fresh quote`);
        continue;
      }
      throw e;
    }
  }
} catch (e) {
  if (e instanceof KobX402Error) {
    console.error(`${e.code}${e.diagnostic ? ` (${e.diagnostic})` : ''}: ${e.message}`);
    if (e.paymentId) console.error(`artifact ${e.paymentId} is stored; reconcile it with client.resume(id) or revoke it with client.revoke(id)`);
    process.exitCode = 1;
  } else {
    throw e;
  }
} finally {
  await rpc.disconnect();
}

/** `KAS=500000000,<covenant id>=900` -> `{ KAS: '500000000', '<covenant id>': '900' }`; a bare number is a KAS bound. */
function parseMaxPay(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const part of text.split(',').map((s) => s.trim()).filter(Boolean)) {
    const [asset, amount] = part.includes('=') ? (part.split('=', 2) as [string, string]) : ['KAS', part];
    out[asset.trim()] = amount.trim();
  }
  return out;
}
