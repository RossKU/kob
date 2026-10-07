// A paid response is replayed only to the payment that settled it: the paywall remembers the transaction a payment id
// was settled with, and a request repeating the id with any other transaction (or none) is not served.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { encodePaymentSignature } from '../src/headers.ts';
import { httpRequestHash, requirementsHash } from '../src/canonical.ts';
import { PAYMENT_IDENTIFIER_KEY, PAYLOAD_EXACT_TX, X402_VERSION } from '../src/types.ts';
import type { PaymentRequired, PaymentRequirements, SettlementResponse } from '../src/types.ts';
import { createPaywall } from '../src/server.ts';
import { MERCHANT, NETWORK, startRig } from './helpers/env.ts';
import { stubWasm } from './helpers/stub-wasm.ts';
import { defaultSettlement } from './helpers/stub-facilitator.ts';

/** A PAYMENT-SIGNATURE for `url` with the given payment id and transaction text (nothing else of a payment). */
function header(offer: PaymentRequirements, url: string, paymentId: string, transaction: string): string {
  return encodePaymentSignature({
    x402Version: X402_VERSION,
    accepted: offer,
    payload: { type: PAYLOAD_EXACT_TX, requestHash: httpRequestHash('GET', url, null, requirementsHash(offer)), transaction },
    extensions: { [PAYMENT_IDENTIFIER_KEY]: { info: { required: true, id: paymentId } } },
  } as never);
}

const otherTx = (id: string): string => JSON.stringify({ id, version: 0, inputs: [{ txid: '77'.repeat(32), index: 0 }], outputs: [] });

test('a settled payment id with another transaction (or none) is not served and does not reach the facilitator', async () => {
  const rig = await startRig();
  try {
    const url = `${rig.base}/report`;
    const paid = await rig.client.paidFetch(url);
    assert.equal(paid.response.status, 200);
    const id = paid.payment!.paymentId;
    const offer = ((await (await fetch(url)).json()) as PaymentRequired).accepts[0]!;
    const settles = rig.facilitator.settleCalls().length;

    const noTx = await fetch(url, { headers: { 'payment-signature': header(offer, url, id, 'not json') } });
    assert.equal(noTx.status, 402, 'a transaction without a declared id is refused');
    const other = await fetch(url, { headers: { 'payment-signature': header(offer, url, id, otherTx('ee'.repeat(32))) } });
    assert.equal(other.status, 409);
    const body = (await other.json()) as { extensions: { kaspa: { diagnostic: string } } };
    assert.equal(body.extensions.kaspa.diagnostic, 'kaspa_payment_identifier_conflict');
    // the settled transaction's id with other bytes is not the same payment either
    const sameIdOtherBytes = await fetch(url, { headers: { 'payment-signature': header(offer, url, id, otherTx(paid.payment!.transactionId)) } });
    assert.equal(sameIdOtherBytes.status, 409);

    assert.equal(rig.facilitator.settleCalls().length, settles, 'nothing was forwarded');
    assert.equal(rig.handled.length, 1, 'the handler ran once, for the payment');

    // the payer's own retry of the stored artifact is still answered from memory
    const again = await rig.client.resume(id).catch((e: Error) => e);
    assert.ok(again instanceof Error, 'resume refuses a settled artifact');
    const stored = (await rig.store.load(id))!.paymentPayload;
    const replay = await fetch(url, { headers: { 'payment-signature': encodePaymentSignature(stored) } });
    assert.equal(replay.status, 200);
    assert.deepEqual(
      rig.handled.map((h) => h.replayed),
      [false, true],
    );
  } finally {
    await rig.close();
  }
});

test('after a paywall restart, a facilitator answer for another transaction under a settled id is not served', async () => {
  // a facilitator that answers every settle under a known id with the first settlement it made
  let first: SettlementResponse | undefined;
  const rig = await startRig({ settle: (_n, fr) => ({ body: (first ??= defaultSettlement(fr)) }) });
  try {
    const url = `${rig.base}/report`;
    const paid = await rig.client.paidFetch(url);
    const id = paid.payment!.paymentId;
    const offer = ((await (await fetch(url)).json()) as PaymentRequired).accepts[0]!;
    const handled: boolean[] = [];
    const restarted = createPaywall({
      wasm: stubWasm(),
      network: NETWORK,
      payTo: MERCHANT,
      offers: [{ kind: 'native', amount: '50000000' }],
      facilitator: { url: rig.facilitator.url, retryDelayMs: 5 },
      publicUrl: rig.base,
      resource: { description: 'Report', mimeType: 'text/plain' },
      handler: (_req, p) => (handled.push(p.replayed), new Response('paid content')),
    });
    const undeclared = await restarted.handle(new Request(url, { headers: { 'payment-signature': header(offer, url, id, '{"version":0}') } }));
    assert.equal(undeclared.status, 402, 'a transaction that does not declare its id is refused before settlement');
    const declared = await restarted.handle(new Request(url, { headers: { 'payment-signature': header(offer, url, id, otherTx('ee'.repeat(32))) } }));
    assert.equal(declared.status, 502, 'the settlement shows another transaction than the one sent');
    assert.deepEqual(handled, []);
    // the payment's own transaction is served (this facilitator gives no repeat mark, so this paywall instance sees it new)
    const stored = (await rig.store.load(id))!.paymentPayload;
    const own = await restarted.handle(new Request(url, { headers: { 'payment-signature': encodePaymentSignature(stored) } }));
    assert.equal(own.status, 200);
    assert.deepEqual(handled, [false]);
  } finally {
    await rig.close();
  }
});
