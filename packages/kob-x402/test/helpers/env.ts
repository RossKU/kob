// Shared fixtures: vectors, a paywall + client wired to a stub facilitator and a stub wasm.

import { readFileSync } from 'node:fs';
import { createServer } from 'node:http';
import type { Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { KobX402Client } from '../../src/client.ts';
import type { ChainContext, ChainContextProvider, KobX402ClientOptions } from '../../src/client.ts';
import { MemoryArtifactStore } from '../../src/artifact-store.ts';
import type { ArtifactStore } from '../../src/artifact-store.ts';
import { createNodeServer, createPaywall } from '../../src/server.ts';
import type { Paywall, PaywallConfig } from '../../src/server.ts';
import type { OfferSpec, PayerUtxo } from '../../src/types.ts';
import { startStubFacilitator } from './stub-facilitator.ts';
import type { SettleScript, StubFacilitator } from './stub-facilitator.ts';
import { stubWasm } from './stub-wasm.ts';
import type { StubWasm } from './stub-wasm.ts';

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, '../../../..');

/** Reads a vendored rc.1 vector (crates/kob-x402/vectors/kaspa-x402-rc1). */
export function loadVector<T = any>(rel: string): T {
  return JSON.parse(readFileSync(join(repo, 'crates/kob-x402/vectors/kaspa-x402-rc1', rel), 'utf8')) as T;
}

export const NETWORK = 'kaspa:testnet-10' as const;
export const MERCHANT = 'kaspatest:qq0merchantmerchantmerchantmerchantmerchantmerchantmerchan';
export const PAYER = 'kaspatest:qpayerpayerpayerpayerpayerpayerpayerpayerpayerpayerpayerpayer';
export const TOKEN_A = 'aa'.repeat(32);
export const TOKEN_B = 'bb'.repeat(32);

export const kasUtxo = (n = 0, amount = '1000000000'): PayerUtxo => ({
  txid: (n + 1).toString(16).padStart(2, '0').repeat(32),
  index: n,
  amount,
  scriptPublicKey: '000020' + '11'.repeat(32) + 'ac',
  blockDaaScore: '100',
  isCoinbase: false,
});

export function staticContext(extra: Partial<ChainContext> = {}): ChainContextProvider & { loads: number } {
  const p = {
    loads: 0,
    async load() {
      p.loads++;
      return {
        utxos: [kasUtxo(0), kasUtxo(1)],
        tokenUtxos: [{ transactionId: '05'.repeat(32), index: 5, amount: '100000000', blockDaaScore: '100', covenantId: TOKEN_A, state: { amount: '100' } }],
        quote: { lockTime: '1000', orders: [{ leg: { kind: 'bid', order: { transactionId: '99'.repeat(32), index: 0, amount: '1', state: {} }, amount: '3000' } }] },
        virtualDaaScore: '1000',
        ...extra,
      };
    },
  };
  return p;
}

/** Explicit spend authorisation (nothing autopays without ceilings): generous, per merchant asset. */
export const SPEND_CAPS = { KAS: '1000000000000', [TOKEN_A]: '1000000000', [TOKEN_B]: '1000000000' };
export const SWAP_MAX_PAY = '1000000000000';
/** Per pay asset swap bounds (every asset the fixtures pay with). */
export const SWAP_BOUNDS = { KAS: SWAP_MAX_PAY, [TOKEN_A]: SWAP_MAX_PAY, [TOKEN_B]: SWAP_MAX_PAY };

export const NATIVE_OFFER: OfferSpec = { kind: 'native', amount: '50000000' };

export interface Rig {
  wasm: StubWasm;
  facilitator: StubFacilitator;
  paywall: Paywall;
  server: Server;
  base: string;
  store: ArtifactStore;
  client: KobX402Client;
  context: ReturnType<typeof staticContext>;
  handled: { paymentId: string; replayed: boolean; url: string; body: string }[];
  close(): Promise<void>;
}

export interface RigOptions {
  offers?: OfferSpec[];
  settle?: SettleScript;
  apiKey?: string;
  capabilities?: KobX402ClientOptions['capabilities'];
  paywall?: Partial<PaywallConfig>;
  client?: Partial<KobX402ClientOptions>;
  store?: ArtifactStore;
}

export async function startRig(o: RigOptions = {}): Promise<Rig> {
  const wasm = stubWasm();
  const facilitator = await startStubFacilitator({ ...(o.apiKey ? { apiKey: o.apiKey } : {}), ...(o.settle ? { settle: o.settle } : {}) });
  const handled: Rig['handled'] = [];
  const paywall = createPaywall({
    wasm,
    network: NETWORK,
    payTo: MERCHANT,
    offers: o.offers ?? [NATIVE_OFFER],
    facilitator: { url: facilitator.url, ...(o.apiKey ? { apiKey: o.apiKey } : {}), retryDelayMs: 5 },
    resource: { description: 'Report', mimeType: 'text/plain' },
    handler: (req, paid) => {
      handled.push({ paymentId: paid.paymentId, replayed: paid.replayed, url: paid.url, body: new TextDecoder().decode(paid.body) });
      return new Response(`report for ${paid.method} ${new URL(req.url).pathname}`, { headers: { 'content-type': 'text/plain' } });
    },
    ...o.paywall,
  });
  const server = createNodeServer(paywall);
  await new Promise<void>((r) => server.listen(0, '127.0.0.1', r));
  const base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  const store = o.store ?? new MemoryArtifactStore();
  const context = staticContext();
  const client = new KobX402Client({
    wasm,
    network: NETWORK,
    payerAddress: PAYER,
    privateKeys: ['01'.repeat(32)],
    context,
    store,
    capabilities: { maxAmount: SPEND_CAPS, ...(o.capabilities ?? {}) },
    maxPay: SWAP_BOUNDS,
    ...o.client,
  });
  return {
    wasm,
    facilitator,
    paywall,
    server,
    base,
    store,
    client,
    context,
    handled,
    close: async () => {
      server.closeAllConnections();
      await new Promise<void>((r) => server.close(() => r()));
      await facilitator.close();
    },
  };
}

/** A tiny raw http server for rogue-merchant tests. */
export async function startRaw(handler: Parameters<typeof createServer>[1]): Promise<{ base: string; close(): Promise<void> }> {
  const server = createServer(handler);
  await new Promise<void>((r) => server.listen(0, '127.0.0.1', r));
  return {
    base: `http://127.0.0.1:${(server.address() as AddressInfo).port}`,
    close: () => new Promise<void>((r) => (server.closeAllConnections(), server.close(() => r()))),
  };
}
