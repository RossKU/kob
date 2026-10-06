// Regression: the paywall's idempotency is keyed by the payment id, which no digest covers; a
// captured PAYMENT-SIGNATURE replayed with another id must not run the handler again as a fresh paid request.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { encodePaymentSignature } from '../src/headers.ts';
import { PAYMENT_IDENTIFIER_KEY } from '../src/types.ts';
import { createPaywall } from '../src/server.ts';
import { MERCHANT, NATIVE_OFFER, NETWORK, startRig } from './helpers/env.ts';
import { stubWasm } from './helpers/stub-wasm.ts';

test('the same signed transaction under a new payment id is a replay of the first payment, not a new paid request', async () => {
  const rig = await startRig();
  try {
    const url = `${rig.base}/report`;
    const first = await rig.client.paidFetch(url);
    assert.equal(first.response.status, 200);
    const captured = (await rig.store.list())[0]!.paymentPayload;
    const firstId = captured.extensions![PAYMENT_IDENTIFIER_KEY]!.info.id as string;
    const forged = structuredClone(captured);
    for (const id of ['attacker-chosen-id-0123456789', 'attacker-chosen-id-abcdefghij']) {
      forged.extensions![PAYMENT_IDENTIFIER_KEY]!.info.id = id;
      const r = await fetch(url, { headers: { 'payment-signature': encodePaymentSignature(forged) } });
      assert.equal(r.status, 200);
      // a second use of the same forged id hits the alias entry: still a replay of the first payment
      const again = await fetch(url, { headers: { 'payment-signature': encodePaymentSignature(forged) } });
      assert.equal(again.status, 200);
    }
    assert.equal(rig.handled.filter((h) => !h.replayed).length, 1, 'exactly one non-replayed execution for one on-chain payment');
    assert.ok(rig.handled.slice(1).every((h) => h.replayed && h.paymentId === firstId), 'replays carry the ORIGINAL payment id');
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
