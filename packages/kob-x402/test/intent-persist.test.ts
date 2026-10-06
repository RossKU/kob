// payInvoiceWithIntent keeps the payer able to cancel (X-2): the intent handle is persisted BEFORE the facilitator is
// called, and a failed submission carries the signed payment on the thrown error. Fake wasm and fake invoice client, no
// network, no real wasm.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { payInvoiceWithIntent } from '../src/intent.ts';
import type { InvoiceClient, FetchedInvoice } from '../src/invoice.ts';
import { KobX402Error } from '../src/errors.ts';
import { ASSET_KAS, BINDING_EXACT, BINDING_INTENT, ROUTER_ARTIFACT_ID, TX_ENCODING } from '../src/types.ts';
import type { PaymentRequirements, SettlementResponse } from '../src/types.ts';

const NOW = 1_800_000_000_000;
const ID = 'ab'.repeat(32);

function intentOffer(): PaymentRequirements {
  return {
    scheme: 'exact',
    network: 'kaspa:testnet-10',
    amount: '500000000',
    asset: ASSET_KAS,
    payTo: 'kaspatest:qmerchant',
    maxTimeoutSeconds: 600,
    extra: {
      binding: BINDING_EXACT,
      profile: 'standard-native',
      finality: 'accepted',
      transactionEncoding: TX_ENCODING,
      payToScriptPublicKey: '0000',
      route: { binding: BINDING_INTENT, critical: true, router: ROUTER_ARTIFACT_ID, payAssets: [{ asset: '70'.repeat(32), templateHash: '40'.repeat(32), extensionCommitment: 'ee'.repeat(32) }] },
    },
  } as PaymentRequirements;
}

const fetched = { invoice: { accepts: [intentOffer()] }, id: ID, expiresAtMs: NOW + 600_000 } as unknown as FetchedInvoice;
const req = { payAsset: '70'.repeat(32), utxos: [], nowMs: NOW, options: { maxSell: '3000' } };
const SETTLED = { success: true, transaction: 'cd'.repeat(32) } as unknown as SettlementResponse;

function fakeWasm(log: string[]) {
  return {
    payIntent(r: any) {
      log.push('sign');
      return { paymentPayload: { x402Version: 2, accepted: r.requirements }, transactionId: 'cd'.repeat(32), consumed: [], feeSompi: '1', expiresAtMs: NOW + 300_000, payerSpent: '3000', intent: { actor: 'TokenToKas_sell', state: {}, intent: {} } };
    },
  } as any;
}

function fakeInvoices(log: string[], outcome: () => Promise<SettlementResponse>) {
  return {
    async pay() {
      log.push('pay');
      return outcome();
    },
  } as unknown as InvoiceClient;
}

test('persist receives the payment and is awaited before the facilitator is called', async () => {
  const log: string[] = [];
  let stored: any;
  const r = await payInvoiceWithIntent(fakeWasm(log), fakeInvoices(log, async () => SETTLED), fetched, req, {
    persist: async (p) => {
      log.push('persist-start');
      await new Promise((res) => setTimeout(res, 20));
      stored = p;
      log.push('persist-end');
    },
  });
  assert.deepEqual(log, ['sign', 'persist-start', 'persist-end', 'pay']);
  assert.equal(stored, r.payment, 'persist got the same payment the call returns');
  assert.equal(stored.intent.actor, 'TokenToKas_sell');
});

test('a failed submission (KobX402Error) carries the signed payment; the same error is rethrown', async () => {
  const log: string[] = [];
  const failure = new KobX402Error('facilitator', 'POST /invoices/x/pay: HTTP 503', { status: 503, details: { error: 'busy' }, retryable: true });
  let stored: any;
  await assert.rejects(
    payInvoiceWithIntent(fakeWasm(log), fakeInvoices(log, async () => { throw failure; }), fetched, req, { persist: (p) => { stored = p; } }),
    (e: unknown) => {
      assert.equal(e, failure, 'the same error object');
      assert.equal((e as KobX402Error).payment, stored, 'the payment equals what persist saw');
      assert.equal((e as KobX402Error).payment!.intent.actor, 'TokenToKas_sell');
      assert.deepEqual((e as KobX402Error).details, { error: 'busy' }, 'details are untouched');
      assert.equal((e as KobX402Error).status, 503);
      return true;
    },
  );
});

test('a failed submission works without persist, and a foreign error is wrapped with the payment', async () => {
  const log: string[] = [];
  const boom = new TypeError('socket hang up');
  await assert.rejects(
    payInvoiceWithIntent(fakeWasm(log), fakeInvoices(log, async () => { throw boom; }), fetched, req),
    (e: unknown) => {
      assert.ok(e instanceof KobX402Error);
      assert.equal(e.code, 'payment_pending');
      assert.equal(e.cause, boom);
      assert.equal(e.payment!.transactionId, 'cd'.repeat(32));
      assert.equal(e.payment!.intent.actor, 'TokenToKas_sell');
      return true;
    },
  );
  assert.deepEqual(log, ['sign', 'pay']);
});

test('a failing persist stops the payment: the facilitator is never called and the error is rethrown', async () => {
  const log: string[] = [];
  const disk = new Error('disk full');
  await assert.rejects(
    payInvoiceWithIntent(fakeWasm(log), fakeInvoices(log, async () => SETTLED), fetched, req, { persist: async () => { throw disk; } }),
    (e: unknown) => e === disk,
  );
  assert.deepEqual(log, ['sign'], 'signed, but nothing was submitted');
});
