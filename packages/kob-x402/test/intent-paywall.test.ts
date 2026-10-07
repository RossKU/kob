// An intent payment is paid by the facilitator's execution of the intent the request's transaction creates: the
// settlement names the execution in `transaction` and the creation in `extensions.kob.intent.creation` (the shape of
// kob-executor's intent settlement). The paywall binds the request to the creation.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { decodePaymentResponse, encodePaymentSignature } from '../src/headers.ts';
import { httpRequestHash, requirementsHash } from '../src/canonical.ts';
import { PAYMENT_IDENTIFIER_KEY, PAYLOAD_EXACT_TX, X402_VERSION } from '../src/types.ts';
import type { FacilitatorRequest, PaymentRequired, SettlementResponse } from '../src/types.ts';
import { settledRequestTransaction } from '../src/server.ts';
import { startRig, TOKEN_A } from './helpers/env.ts';
import { defaultSettlement } from './helpers/stub-facilitator.ts';

const CREATION = 'c0'.repeat(32);
const EXECUTION = 'e0'.repeat(32);
const INTENT_OFFER = { kind: 'swap', mode: 'intent', receive: 'kas', amount: '200000000', payAssets: [{ asset: TOKEN_A }] } as never;

function intentSettlement(fr: FacilitatorRequest, creation: string, extra: Record<string, unknown> = {}): SettlementResponse {
  const s = defaultSettlement(fr);
  return { ...s, transaction: EXECUTION, extensions: { kaspa: s.extensions!.kaspa, kob: { intent: { creation, executions: 1 }, ...extra } } };
}

async function pay(creation: string, settle: (fr: FacilitatorRequest) => SettlementResponse, id = 'intent-payer-id-0000001') {
  const rig = await startRig({ offers: [INTENT_OFFER], settle: (_n, fr) => ({ body: settle(fr) }) });
  const url = `${rig.base}/report`;
  const offer = ((await (await fetch(url)).json()) as PaymentRequired).accepts[0]!;
  assert.equal((offer.extra as { route?: { binding?: string } }).route?.binding, 'kob-intent-v1');
  const header = encodePaymentSignature({
    x402Version: X402_VERSION,
    accepted: offer,
    payload: { type: PAYLOAD_EXACT_TX, requestHash: httpRequestHash('GET', url, null, requirementsHash(offer)), transaction: JSON.stringify({ id: creation, version: 0 }) },
    extensions: { [PAYMENT_IDENTIFIER_KEY]: { info: { required: true, id } } },
  } as never);
  const r = await fetch(url, { headers: { 'payment-signature': header } });
  return { rig, r, header, url };
}

test('an intent-paid resource is served: the settlement names the execution and the request carried the creation', async () => {
  const { rig, r, header, url } = await pay(CREATION, (fr) => intentSettlement(fr, CREATION));
  try {
    assert.equal(r.status, 200, await r.clone().text());
    assert.equal(rig.handled.length, 1);
    const receipt = decodePaymentResponse(r.headers.get('payment-response')!);
    assert.equal(receipt.transaction, EXECUTION, 'the transaction that paid the merchant');
    assert.equal(rig.handled[0]!.replayed, false);
    // the payer's retry is answered from memory
    const again = await fetch(url, { headers: { 'payment-signature': header } });
    assert.equal(again.status, 200);
    assert.deepEqual(rig.handled.map((h) => h.replayed), [false, true]);
    assert.equal(rig.facilitator.settleCalls().length, 1);
  } finally {
    await rig.close();
  }
});

test('an intent settlement for another creation, or without one, is not served', async () => {
  for (const settle of [
    (fr: FacilitatorRequest) => intentSettlement(fr, 'c1'.repeat(32)),
    (fr: FacilitatorRequest) => ({ ...defaultSettlement(fr), transaction: EXECUTION }),
    // the creation itself named as the paying transaction, without the intent extension
    (fr: FacilitatorRequest) => defaultSettlement(fr),
  ]) {
    const { rig, r } = await pay(CREATION, settle);
    try {
      assert.equal(r.status, 502);
      assert.equal(rig.handled.length, 0);
    } finally {
      await rig.close();
    }
  }
});

test('settledRequestTransaction reads the creation of an intent settlement and the transaction of a direct one', () => {
  const s = { success: true, transaction: EXECUTION.toUpperCase(), extensions: { kob: { intent: { creation: CREATION.toUpperCase() } } } } as SettlementResponse;
  assert.equal(settledRequestTransaction(s, true), CREATION);
  assert.equal(settledRequestTransaction(s, false), EXECUTION);
  assert.equal(settledRequestTransaction({ ...s, extensions: {} }, true), undefined);
});
