// Audit 2026-10-30 X2: a settled payment is re-served (a lost answer is resent) only inside its retry window, not for ever.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { encodePaymentSignature } from '../src/headers.ts';
import { createPaywall } from '../src/server.ts';
import { MERCHANT, NATIVE_OFFER, NETWORK, startRig } from './helpers/env.ts';
import { stubFacilitatorClock } from './helpers/stub-facilitator.ts';
import { stubWasm } from './helpers/stub-wasm.ts';

test('a repeat of a settled payment is served inside the retry window and asked to pay again after it', async () => {
  const clock = { t: Date.now() };
  const rig = await startRig({ paywall: { now: () => clock.t } });
  try {
    const url = `${rig.base}/report`;
    const first = await rig.client.paidFetch(url);
    assert.equal(first.response.status, 200);
    const captured = (await rig.store.load(first.payment!.paymentId))!.paymentPayload;
    const windowMs = captured.accepted.maxTimeoutSeconds * 1000;
    const settles = rig.facilitator.settleCalls().length;
    const repeat = () => fetch(url, { headers: { 'payment-signature': encodePaymentSignature(captured) } });

    clock.t += windowMs - 1;
    const inside = await repeat();
    assert.equal(inside.status, 200);
    await inside.text();

    clock.t += 2;
    const late = await repeat();
    assert.equal(late.status, 402);
    const body = (await late.json()) as { extensions?: { kaspa?: { diagnostic?: string } } };
    assert.equal(body.extensions?.kaspa?.diagnostic, 'payment_retry_window_passed');
    assert.equal(rig.facilitator.settleCalls().length, settles, 'the facilitator was not asked again');
    assert.equal(rig.handled.length, 2, 'the handler ran for the payment and the repeat inside the window only');
  } finally {
    await rig.close();
  }
});

// Audit 2026-10-30 X10: a paywall that forgot the payment (a restart, an eviction, a second instance) asks the facilitator, which says how
// long ago it first answered; the retry window applies to that age too.
test('a restarted paywall does not serve a repeat the facilitator first answered longer ago than the retry window', async () => {
  const rig = await startRig();
  try {
    const url = `${rig.base}/report`;
    const first = await rig.client.paidFetch(url);
    assert.equal(first.response.status, 200);
    const captured = (await rig.store.load(first.payment!.paymentId))!.paymentPayload;
    const windowMs = captured.accepted.maxTimeoutSeconds * 1000;
    const handled: boolean[] = [];
    const restarted = () =>
      createPaywall({
        wasm: stubWasm(),
        network: NETWORK,
        payTo: MERCHANT,
        offers: [NATIVE_OFFER],
        facilitator: { url: rig.facilitator.url, retryDelayMs: 5 },
        publicUrl: rig.base,
        resource: { description: 'Report', mimeType: 'text/plain' },
        handler: (_req, p) => (handled.push(p.replayed), new Response('paid content')),
      });
    const repeat = (p: ReturnType<typeof restarted>) => p.handle(new Request(url, { headers: { 'payment-signature': encodePaymentSignature(captured) } }));
    stubFacilitatorClock.skewMs = 0;
    const inside = await repeat(restarted());
    assert.equal(inside.status, 200);
    assert.deepEqual(handled, [true]);
    stubFacilitatorClock.skewMs = windowMs + 1;
    const late = await repeat(restarted());
    assert.equal(late.status, 402);
    const body = (await late.json()) as { extensions?: { kaspa?: { diagnostic?: string } } };
    assert.equal(body.extensions?.kaspa?.diagnostic, 'payment_retry_window_passed');
    assert.deepEqual(handled, [true], 'the handler did not run for the late repeat');
  } finally {
    stubFacilitatorClock.skewMs = 0;
    await rig.close();
  }
});
