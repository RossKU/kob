// The paywall's memory of settled payments holds settled payments only: requests that are being settled are kept apart in a
// bounded set, so any number of well-formed requests that never settle cannot push a settled payment out of it.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { encodePaymentSignature } from '../src/headers.ts';
import { httpRequestHash, requirementsHash } from '../src/canonical.ts';
import { PAYMENT_IDENTIFIER_KEY, PAYLOAD_EXACT_TX, X402_VERSION } from '../src/types.ts';
import type { PaymentRequired, PaymentRequirements } from '../src/types.ts';
import { startRig } from './helpers/env.ts';
import { failure } from './helpers/stub-facilitator.ts';

function header(offer: PaymentRequirements, url: string, paymentId: string, txid: string): string {
  return encodePaymentSignature({
    x402Version: X402_VERSION,
    accepted: offer,
    payload: { type: PAYLOAD_EXACT_TX, requestHash: httpRequestHash('GET', url, null, requirementsHash(offer)), transaction: JSON.stringify({ id: txid, version: 0 }) },
    extensions: { [PAYMENT_IDENTIFIER_KEY]: { info: { required: true, id: paymentId } } },
  } as never);
}

const txidOf = (fr: { paymentPayload: { payload: { transaction: string } } }): string => (JSON.parse(fr.paymentPayload.payload.transaction) as { id: string }).id;

test('requests that never settle do not push a settled payment out of the paywall memory', async () => {
  let paid: string | undefined;
  const rig = await startRig({
    paywall: { maxLedgerEntries: 2 },
    // the first payment settles; every other transaction is refused by the facilitator
    settle: (_n, fr) => ((paid ??= txidOf(fr)) === txidOf(fr) ? undefined : { body: failure('invalid_kaspa_exact_signature', false, 'unsigned') }),
  });
  try {
    const url = `${rig.base}/report`;
    const first = await rig.client.paidFetch(url);
    assert.equal(first.response.status, 200);
    const offer = ((await (await fetch(url)).json()) as PaymentRequired).accepts[0]!;
    for (let i = 0; i < 20; i++) {
      const r = await fetch(url, { headers: { 'payment-signature': header(offer, url, `unsettled-request-${String(i).padStart(4, '0')}`, (i + 16).toString(16).padStart(2, '0').repeat(32)) } });
      assert.equal(r.status, 402);
    }
    const settles = rig.facilitator.settleCalls().length;
    // the payer's retry is still answered from memory, as a repeat, without the facilitator
    const stored = (await rig.store.load(first.payment!.paymentId))!.paymentPayload;
    const again = await fetch(url, { headers: { 'payment-signature': encodePaymentSignature(stored) } });
    assert.equal(again.status, 200);
    assert.equal(rig.facilitator.settleCalls().length, settles);
    assert.deepEqual(rig.handled.map((h) => h.replayed), [false, true]);
  } finally {
    await rig.close();
  }
});

test('the set of payments being settled is bounded: beyond it a request is answered 503 and not remembered', async () => {
  const rig = await startRig({ paywall: { maxSettling: 1 }, settle: () => ({ delayMs: 300 }) });
  try {
    const url = `${rig.base}/report`;
    const offer = ((await (await fetch(url)).json()) as PaymentRequired).accepts[0]!;
    const slow = fetch(url, { headers: { 'payment-signature': header(offer, url, 'settling-request-0001', 'a1'.repeat(32)) } });
    await new Promise((r) => setTimeout(r, 50));
    const busy = await fetch(url, { headers: { 'payment-signature': header(offer, url, 'settling-request-0002', 'a2'.repeat(32)) } });
    assert.equal(busy.status, 503);
    assert.equal((await slow).status, 200);
    // once the first one ended, the second id is free (it was never remembered)
    const later = await fetch(url, { headers: { 'payment-signature': header(offer, url, 'settling-request-0002', 'a2'.repeat(32)) } });
    assert.equal(later.status, 200);
  } finally {
    await rig.close();
  }
});
