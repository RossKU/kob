// Regression: the paywall's idempotency is keyed by the payment id, which no digest covers; a settled transaction
// (public once broadcast) presented under another id is not served, before or after a paywall restart.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { encodePaymentSignature } from '../src/headers.ts';
import { PAYMENT_IDENTIFIER_KEY } from '../src/types.ts';
import { createPaywall } from '../src/server.ts';
import { MERCHANT, NATIVE_OFFER, NETWORK, startRig } from './helpers/env.ts';
import { stubWasm } from './helpers/stub-wasm.ts';

test('the same signed transaction under a new payment id is not served', async () => {
  const rig = await startRig();
  try {
    const url = `${rig.base}/report`;
    const first = await rig.client.paidFetch(url);
    assert.equal(first.response.status, 200);
    const captured = (await rig.store.list())[0]!.paymentPayload;
    const firstId = captured.extensions![PAYMENT_IDENTIFIER_KEY]!.info.id as string;
    const settles = rig.facilitator.settleCalls().length;
    const other = structuredClone(captured);
    for (const id of ['other-chosen-id-0123456789', 'other-chosen-id-abcdefghij']) {
      other.extensions![PAYMENT_IDENTIFIER_KEY]!.info.id = id;
      for (let i = 0; i < 2; i++) {
        const r = await fetch(url, { headers: { 'payment-signature': encodePaymentSignature(other) } });
        assert.equal(r.status, 409);
        const body = (await r.json()) as { extensions: { kaspa: { diagnostic: string } } };
        assert.equal(body.extensions.kaspa.diagnostic, 'kaspa_payment_identifier_conflict');
      }
    }
    assert.equal(rig.facilitator.settleCalls().length, settles, 'refused before the facilitator');
    // the payer's own retry is still served, as a repeat
    const own = await fetch(url, { headers: { 'payment-signature': encodePaymentSignature(captured) } });
    assert.equal(own.status, 200);
    assert.deepEqual(rig.handled.map((h) => [h.paymentId, h.replayed]), [[firstId, false], [firstId, true]]);
  } finally {
    await rig.close();
  }
});

test('after a paywall restart, a settled transaction is served only to its own payment id, and as a repeat', async () => {
  const rig = await startRig();
  try {
    const url = `${rig.base}/report`;
    const paid = await rig.client.paidFetch(url);
    assert.equal(paid.response.status, 200);
    const captured = (await rig.store.load(paid.payment!.paymentId))!.paymentPayload;
    const handled: Array<[string, boolean]> = [];
    const restarted = createPaywall({
      wasm: stubWasm(),
      network: NETWORK,
      payTo: MERCHANT,
      offers: [NATIVE_OFFER],
      facilitator: { url: rig.facilitator.url, retryDelayMs: 5 },
      publicUrl: rig.base,
      resource: { description: 'Report', mimeType: 'text/plain' },
      handler: (_req, p) => (handled.push([p.paymentId, p.replayed]), new Response('paid content')),
    });
    // the transaction as anyone reads it from the chain, under another id: the facilitator's binding refuses it
    const other = structuredClone(captured);
    other.extensions![PAYMENT_IDENTIFIER_KEY]!.info.id = 'other-chosen-id-after-restart';
    const r = await restarted.handle(new Request(url, { headers: { 'payment-signature': encodePaymentSignature(other) } }));
    assert.equal(r.status, 402);
    const body = (await r.json()) as { extensions: { kaspa: { diagnostic: string } } };
    assert.equal(body.extensions.kaspa.diagnostic, 'kaspa_payment_identifier_conflict');
    assert.deepEqual(handled, []);
    // the payer's own retry is served, and the handler sees a repeat, not a new payment
    const own = await restarted.handle(new Request(url, { headers: { 'payment-signature': encodePaymentSignature(captured) } }));
    assert.equal(own.status, 200);
    assert.deepEqual(handled, [[paid.payment!.paymentId, true]]);
  } finally {
    await rig.close();
  }
});

test('`//host/path` request paths cannot move the hashed / advertised resource URL', async () => {
  const paywall = createPaywall({
    wasm: stubWasm(),
    network: NETWORK,
    payTo: MERCHANT,
    offers: [NATIVE_OFFER],
    facilitator: { url: 'http://127.0.0.1:1' },
    publicUrl: 'https://merchant.example',
    handler: () => new Response('ok'),
  });
  const res = async (u: string) => ((await paywall.handle(new Request(u))).json() as Promise<{ resource: { url: string } }>).then((j) => j.resource.url);
  assert.equal(await res('http://10.0.0.5:8080/report'), 'https://merchant.example/report');
  assert.equal(await res('http://10.0.0.5:8080//evil.example/report'), 'https://merchant.example//evil.example/report', 'stays on the merchant origin');
  assert.notEqual(await res('http://10.0.0.5:8080//merchant.example/report'), 'https://merchant.example/report', 'no aliasing of /report');
});
