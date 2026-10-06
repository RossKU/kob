// The payer against hostile or broken merchants: tampered PAYMENT-RESPONSE, redirects, malformed 402s.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { encodePaymentRequired, encodePaymentResponse, decodePaymentSignature } from '../src/headers.ts';
import { KobX402Client } from '../src/client.ts';
import { KobX402Error } from '../src/errors.ts';
import { MemoryArtifactStore } from '../src/artifact-store.ts';
import { buildOffer } from '../src/offers.ts';
import type { PaymentRequired, SettlementResponse } from '../src/types.ts';
import { MERCHANT, NATIVE_OFFER, NETWORK, PAYER, SPEND_CAPS, staticContext, startRaw } from './helpers/env.ts';
import { stubWasm } from './helpers/stub-wasm.ts';

const wasm = stubWasm();
const offer = buildOffer(NATIVE_OFFER, { wasm, network: NETWORK, payTo: MERCHANT, maxTimeoutSeconds: 60, finality: 'accepted' });

function pr(url: string): PaymentRequired {
  return { x402Version: 2, resource: { url }, accepts: [offer] };
}

function client(store = new MemoryArtifactStore()) {
  return new KobX402Client({ wasm: stubWasm(), network: NETWORK, payerAddress: PAYER, privateKeys: ['01'.repeat(32)], context: staticContext(), store, capabilities: { maxAmount: SPEND_CAPS } });
}

/** A merchant that issues a real 402 and answers the paid retry with whatever `paidAnswer` builds from the signed artifact. */
async function rogue(paidAnswer: (txid: string, res: import('node:http').ServerResponse, url: string) => void) {
  let base = '';
  const srv = await startRaw((req, res) => {
    const url = base + (req.url ?? '/');
    const sig = req.headers['payment-signature'];
    if (typeof sig !== 'string') {
      res.writeHead(402, { 'payment-required': encodePaymentRequired(pr(url)), 'content-type': 'application/json' }).end(JSON.stringify(pr(url)));
      return;
    }
    const payload = decodePaymentSignature(sig);
    const txid = (JSON.parse(payload.payload.transaction) as { id: string }).id;
    paidAnswer(txid, res, url);
  });
  base = srv.base;
  return srv;
}

const good = (txid: string): SettlementResponse => ({ success: true, transaction: txid, network: NETWORK, amount: offer.amount, payer: PAYER });
const ok = (res: import('node:http').ServerResponse, s: SettlementResponse): void => {
  res.writeHead(200, { 'payment-response': encodePaymentResponse(s), 'content-type': 'text/plain' }).end('resource');
};

test('a correct PAYMENT-RESPONSE is accepted by the same rogue harness (control)', async () => {
  const srv = await rogue((txid, res) => ok(res, good(txid)));
  try {
    const r = await client().fetch(srv.base + '/r');
    assert.equal(await r.text(), 'resource');
  } finally {
    await srv.close();
  }
});

test('a tampered PAYMENT-RESPONSE is rejected: wrong transaction id, wrong amount, wrong network, failure, unparsable, missing', async () => {
  const cases: [string, (txid: string) => SettlementResponse | string | undefined][] = [
    ['wrong txid', () => good('ab'.repeat(32))],
    ['txid not a hash', () => good('nope')],
    ['wrong amount', (t) => ({ ...good(t), amount: '1' })],
    ['missing amount', (t) => ({ ...good(t), amount: undefined })],
    ['wrong network', (t) => ({ ...good(t), network: 'kaspa:mainnet' })],
    ['success false', (t) => ({ ...good(t), success: false })],
    ['header not base64', () => '%%%not-base64%%%'],
    ['no header', () => undefined],
  ];
  for (const [name, make] of cases) {
    const store = new MemoryArtifactStore();
    const srv = await rogue((txid, res) => {
      const v = make(txid);
      if (v === undefined) res.writeHead(200).end('resource-without-receipt');
      else res.writeHead(200, { 'payment-response': typeof v === 'string' ? v : encodePaymentResponse(v) }).end('resource');
    });
    try {
      await assert.rejects(client(store).fetch(srv.base + '/r'), (e: unknown) => e instanceof KobX402Error && (e.code === 'invalid_settlement' || e.code === 'payment_pending'), name);
      const rec = (await store.list())[0]!;
      assert.equal(rec.status, 'pending', `${name}: the disclosed artifact stays pending, never settled`);
    } finally {
      await srv.close();
    }
  }
});

test('redirect on the challenge request is rejected before anything is signed', async () => {
  const store = new MemoryArtifactStore();
  const other = await startRaw((_req, res) => res.writeHead(402, { 'payment-required': encodePaymentRequired(pr('http://elsewhere/')) }).end('{}'));
  const srv = await startRaw((_req, res) => res.writeHead(302, { location: other.base + '/steal' }).end());
  const c = client(store);
  try {
    await assert.rejects(c.fetch(srv.base + '/r'), (e: unknown) => e instanceof KobX402Error && e.code === 'redirect');
    assert.equal((await store.list()).length, 0);
  } finally {
    await srv.close();
    await other.close();
  }
});

test('redirect on the paid retry is rejected: the signature is never followed to another origin, the artifact stays pending', async () => {
  const store = new MemoryArtifactStore();
  let leaked = false;
  const other = await startRaw((req, res) => {
    if (req.headers['payment-signature']) leaked = true;
    res.writeHead(200).end('x');
  });
  let base = '';
  const srv = await startRaw((req, res) => {
    const url = base + (req.url ?? '/');
    if (!req.headers['payment-signature']) return void res.writeHead(402, { 'payment-required': encodePaymentRequired(pr(url)) }).end('{}');
    res.writeHead(307, { location: other.base + '/steal' }).end();
  });
  base = srv.base;
  try {
    await assert.rejects(client(store).fetch(srv.base + '/r'), (e: unknown) => e instanceof KobX402Error && e.code === 'payment_pending');
    assert.equal(leaked, false);
    assert.equal((await store.list())[0]!.status, 'pending');
  } finally {
    await srv.close();
    await other.close();
  }
});

test('an effective URL that differs from the requested URL is rejected (custom fetch that hides redirects)', async () => {
  const store = new MemoryArtifactStore();
  const c = new KobX402Client({
    wasm: stubWasm(),
    network: NETWORK,
    payerAddress: PAYER,
    privateKeys: ['01'.repeat(32)],
    context: staticContext(),
    store,
    capabilities: { maxAmount: SPEND_CAPS },
    fetch: async (u) => {
      const r = new Response('{}', { status: 402 });
      Object.defineProperty(r, 'url', { value: 'https://evil.example/' + u });
      return r;
    },
  });
  await assert.rejects(c.fetch('https://merchant.example/r'), (e: unknown) => e instanceof KobX402Error && e.code === 'redirect');
  const noUrl = new KobX402Client({ wasm: stubWasm(), network: NETWORK, payerAddress: PAYER, privateKeys: ['01'.repeat(32)], context: staticContext(), capabilities: { maxAmount: SPEND_CAPS }, fetch: async () => new Response('{}', { status: 402 }) });
  await assert.rejects(noUrl.fetch('https://merchant.example/r'), (e: unknown) => e instanceof KobX402Error && e.code === 'redirect', 'no effective URL: fail closed');
});

test('malformed 402 envelopes are errors, not payments', async () => {
  for (const body of ['not json', '{"x402Version":1}', '{"x402Version":2,"resource":{"url":"x"},"accepts":"no"}']) {
    const srv = await startRaw((_req, res) => res.writeHead(402).end(body));
    try {
      await assert.rejects(client().fetch(srv.base + '/r'), (e: unknown) => e instanceof KobX402Error && e.code === 'invalid_payment_required', body);
    } finally {
      await srv.close();
    }
  }
});

test('a foreign-only 402 selects nothing and signs nothing', async () => {
  const store = new MemoryArtifactStore();
  const foreign = { x402Version: 2, resource: { url: 'x' }, accepts: [{ scheme: 'exact', network: 'eip155:8453', amount: '1', asset: '0x0', payTo: '0x1', maxTimeoutSeconds: 60, extra: {} }] };
  const srv = await startRaw((_req, res) => res.writeHead(402, { 'payment-required': encodePaymentRequired(foreign as unknown as PaymentRequired) }).end('{}'));
  try {
    await assert.rejects(client(store).fetch(srv.base + '/r'), (e: unknown) => e instanceof KobX402Error && e.code === 'no_acceptable_offer');
    assert.equal((await store.list()).length, 0);
  } finally {
    await srv.close();
  }
});

test('the client never reuses the merchant-supplied requestHash: it derives its own', async () => {
  // A merchant that puts a requestHash into the offer's extra cannot steer the fingerprint: the hash covers the whole offer.
  let seen = '';
  let base = '';
  const srv = await startRaw((req, res) => {
    const url = base + (req.url ?? '/');
    const sig = req.headers['payment-signature'];
    if (typeof sig !== 'string') {
      const withHash = { ...pr(url), accepts: [{ ...offer, extra: { ...offer.extra, requestHash: '00'.repeat(32) } }] };
      return void res.writeHead(402, { 'payment-required': encodePaymentRequired(withHash as PaymentRequired) }).end('{}');
    }
    seen = decodePaymentSignature(sig).payload.requestHash;
    res.writeHead(500).end();
  });
  base = srv.base;
  try {
    await assert.rejects(client().fetch(srv.base + '/r'));
    assert.match(seen, /^[0-9a-f]{64}$/);
    assert.notEqual(seen, '00'.repeat(32));
  } finally {
    await srv.close();
  }
});
