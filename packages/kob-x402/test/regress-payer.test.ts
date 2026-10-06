// Regression: the payer client against a hostile merchant.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { IncomingMessage, ServerResponse } from 'node:http';
import { encodePaymentRequired } from '../src/headers.ts';
import { KobX402Client } from '../src/client.ts';
import { KobX402Error } from '../src/errors.ts';
import { MemoryArtifactStore } from '../src/artifact-store.ts';
import { buildOffer } from '../src/offers.ts';
import type { OfferSpec, PaymentRequired } from '../src/types.ts';
import { MERCHANT, NATIVE_OFFER, NETWORK, PAYER, TOKEN_A, TOKEN_B, kasUtxo, startRaw, staticContext } from './helpers/env.ts';
import { stubWasm } from './helpers/stub-wasm.ts';

const wasm = stubWasm();
const mk = (spec: OfferSpec) => buildOffer(spec, { wasm, network: NETWORK, payTo: MERCHANT, maxTimeoutSeconds: 60, finality: 'accepted' });
const challengeFor = (spec: OfferSpec) => async (req: IncomingMessage, res: ServerResponse) => {
  const url = `http://${req.headers.host}${req.url}`;
  const pr: PaymentRequired = { x402Version: 2, resource: { url }, accepts: [mk(spec)] };
  res.writeHead(402, { 'payment-required': encodePaymentRequired(pr) }).end('{}');
};
const base = { network: NETWORK, payerAddress: PAYER, privateKeys: ['01'.repeat(32)] };
const kasOnly = { load: async () => ({ utxos: [kasUtxo(0)] }) };

test('an absurd amount is NOT handed to the signer when no ceiling is configured', async () => {
  const w = stubWasm();
  const srv = await startRaw(challengeFor({ ...NATIVE_OFFER, amount: '18446744073709551615' }));
  try {
    const c = new KobX402Client({ ...base, wasm: w, context: kasOnly, store: new MemoryArtifactStore() });
    await assert.rejects(c.fetch(srv.base + '/x'), (e: unknown) => e instanceof KobX402Error && e.code === 'spend_not_authorized');
    assert.equal(w.calls.filter((x) => x.method === 'payNative').length, 0);
    // a ceiling below the offer: the offer is not even payable
    const low = new KobX402Client({ ...base, wasm: w, context: kasOnly, capabilities: { maxAmount: { KAS: '1000' } } });
    await assert.rejects(low.fetch(srv.base + '/x'), (e: unknown) => e instanceof KobX402Error && e.code === 'no_acceptable_offer');
    assert.equal(w.calls.filter((x) => x.method === 'payNative').length, 0);
  } finally {
    await srv.close();
  }
});

test('the approve hook is the user / agent policy: it sees the offer and the reasons, and its "no" pays nothing', async () => {
  const w = stubWasm();
  const srv = await startRaw(challengeFor(NATIVE_OFFER));
  try {
    const seen: string[] = [];
    const deny = new KobX402Client({
      ...base,
      wasm: w,
      context: kasOnly,
      approve: (a) => {
        seen.push(...a.reasons, a.offer.requirements.amount);
        return false;
      },
    });
    await assert.rejects(deny.fetch(srv.base + '/x'), (e: unknown) => e instanceof KobX402Error && e.code === 'spend_not_authorized');
    assert.deepEqual(seen, ['no_spend_cap', '50000000']);
    assert.equal(w.calls.filter((x) => x.method === 'payNative').length, 0);
    const allow = new KobX402Client({ ...base, wasm: w, context: kasOnly, approve: () => true });
    await allow.fetch(srv.base + '/x').catch(() => undefined); // the raw merchant answers 402 again; only the signing matters
    assert.equal(w.calls.filter((x) => x.method === 'payNative').length, 1);
  } finally {
    await srv.close();
  }
});

test('a swap-and-pay route needs maxPayAmount (or approval): a KAS-only payer cannot be sold tokens at any price', async () => {
  const w = stubWasm();
  const spec: OfferSpec = { kind: 'swap', receive: 'kcc20', amount: '700', asset: TOKEN_B, token: { custody: 'unconditional' }, payAssets: [{ asset: 'KAS' }] };
  const srv = await startRaw(challengeFor(spec));
  try {
    const caps = { maxAmount: { [TOKEN_B]: '1000' } };
    const c = new KobX402Client({ ...base, wasm: w, context: staticContext(), capabilities: caps });
    await assert.rejects(c.fetch(srv.base + '/x'), (e: unknown) => e instanceof KobX402Error && e.code === 'spend_not_authorized');
    assert.equal(w.calls.filter((x) => x.method === 'paySwap').length, 0);
    const ok = new KobX402Client({ ...base, wasm: w, context: staticContext(), capabilities: caps, maxPayAmount: '5000000000' });
    await ok.fetch(srv.base + '/x').catch(() => undefined);
    const call = w.calls.find((x) => x.method === 'paySwap')!;
    assert.equal((call.arg as { maxPayAmount?: string }).maxPayAmount, '5000000000');
  } finally {
    await srv.close();
  }
});

test('the carrier ceiling reaches the builder and the preflight', async () => {
  const w = stubWasm();
  const spec: OfferSpec = { kind: 'kcc20', asset: TOKEN_A, amount: '1', token: { custody: 'unconditional', carrier: '50000000000' } };
  const srv = await startRaw(challengeFor(spec));
  try {
    const c = new KobX402Client({
      ...base,
      wasm: w,
      context: staticContext(),
      capabilities: { maxAmount: { [TOKEN_A]: '100' }, tokens: { [TOKEN_A]: '100' } },
      maxCarrierSompi: '200000000',
    });
    await c.fetch(srv.base + '/x').catch(() => undefined);
    const pay = w.calls.find((x) => x.method === 'payKcc20')!.arg as { maxCarrierSompi?: string };
    assert.equal(pay.maxCarrierSompi, '200000000');
    const pf = w.calls.find((x) => x.method === 'preflight')!.arg as { maxCarrierSompi?: string };
    assert.equal(pf.maxCarrierSompi, '200000000');
  } finally {
    await srv.close();
  }
});

test('a merchant that never answers cannot stall the payer (request timeout)', async () => {
  const srv = await startRaw((req, res) => {
    res.writeHead(402, { 'content-type': 'application/json' });
    res.write('{"x402Version":2,'); // never finished
    void req;
  });
  try {
    const c = new KobX402Client({ ...base, wasm: stubWasm(), context: kasOnly, requestTimeoutMs: 300 });
    const t0 = Date.now();
    await assert.rejects(c.fetch(srv.base + '/x'));
    assert.ok(Date.now() - t0 < 5000, 'aborted by the timeout');
  } finally {
    await srv.close();
  }
});

test('a huge body is cut off at the cap, not buffered whole', async () => {
  const CHUNK = 1024 * 1024;
  let pulled = 0;
  const f = async (url: string): Promise<Response> => {
    const body = new ReadableStream<Uint8Array>({
      pull(ctrl) {
        pulled++;
        if (pulled > 200) return ctrl.close();
        ctrl.enqueue(new Uint8Array(CHUNK).fill(0x20));
      },
    });
    const r = new Response(body, { status: 402 });
    Object.defineProperty(r, 'url', { value: url });
    return r;
  };
  const c = new KobX402Client({ ...base, wasm: stubWasm(), context: kasOnly, fetch: f });
  const e = await c.fetch('https://merchant.example/x').catch((x) => x);
  assert.equal((e as KobX402Error).code, 'invalid_payment_required');
  assert.ok(pulled <= 4, `only ${pulled} chunks were read`);
});
