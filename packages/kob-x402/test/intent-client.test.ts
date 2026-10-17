// KobX402Client pays intent offers (`kob-intent-v1`): the payer signs one creation on its own terms (`intentTerms`, held to
// the `maxPay` bound of the pay asset), the paywall's facilitator executes it, and the PAYMENT-RESPONSE names the execution in
// `transaction` and the creation the client signed in `extensions.kob.intent.creation`. A creation is re-sent while its outcome
// is unknown or it may still execute, never re-signed (docs/spec/x402-retry.md section 5).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { KobX402Client, verifySettlement } from '../src/client.ts';
import type { IntentTermsRequest, PaymentApproval } from '../src/client.ts';
import { MemoryArtifactStore } from '../src/artifact-store.ts';
import { encodePaymentResponse } from '../src/headers.ts';
import { classifyOffer } from '../src/offers.ts';
import { KobX402Error } from '../src/errors.ts';
import { HEADER_PAYMENT_REQUIRED, ROUTER_ARTIFACT_ID } from '../src/types.ts';
import type { FacilitatorRequest, OfferSpec, PaymentRequired, SettlementResponse } from '../src/types.ts';
import { NETWORK, PAYER, SPEND_CAPS, SWAP_BOUNDS, TOKEN_A, completePolicy, startRaw, startRig, staticContext } from './helpers/env.ts';
import { defaultSettlement, failure } from './helpers/stub-facilitator.ts';
import { stubWasm } from './helpers/stub-wasm.ts';

const INTENT: OfferSpec = { kind: 'swap', mode: 'intent', receive: 'kas', amount: '200000000', payAssets: [{ asset: TOKEN_A }] };
const HOLDS = { tokens: { [TOKEN_A]: '1000000' } };
const FAST = { baseDelayMs: 1, maxDelayMs: 2, sleep: async () => undefined };

/** The facilitator's answer for an intent payment: the execution in `transaction`, the creation in `extensions.kob.intent`. */
function intentSettlement(fr: FacilitatorRequest, creation?: string): SettlementResponse {
  const s = defaultSettlement(fr);
  return { ...s, transaction: 'e0'.repeat(32), extensions: { kaspa: s.extensions!.kaspa, kob: { intent: { creation: creation ?? s.transaction, executions: 1 } } } };
}

test('an intent offer is classified only for the router this SDK knows, as a critical route', () => {
  const offer = {
    scheme: 'exact',
    network: NETWORK,
    amount: '5',
    asset: 'KAS',
    payTo: 'kaspatest:qmerchant',
    maxTimeoutSeconds: 60,
    extra: { binding: 'kaspa-exact-v2', profile: 'standard-native', paymentFlow: 'upfront', finality: 'accepted', transactionEncoding: 'kaspa-sdk-safe-json-v2.0.0', payToScriptPublicKey: '0000', route: { binding: 'kob-intent-v1', critical: true, router: ROUTER_ARTIFACT_ID, payAssets: [{ asset: TOKEN_A }] } },
  };
  assert.equal(classifyOffer(offer)?.kind, 'intent');
  assert.equal(classifyOffer({ ...offer, extra: { ...offer.extra, route: { ...offer.extra.route, router: 'ab'.repeat(32) } } }), null, 'another intent program');
  assert.equal(classifyOffer({ ...offer, extra: { ...offer.extra, route: { ...offer.extra.route, critical: false } } }), null);
});

test('without intentTerms an intent offer is not paid (and nothing is built)', async () => {
  const rig = await startRig({ offers: [INTENT], capabilities: HOLDS });
  try {
    await assert.rejects(rig.client.fetch(`${rig.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'spend_not_authorized');
    assert.equal(rig.wasm.calls.filter((c) => c.method === 'payIntent').length, 0);
    assert.equal(rig.facilitator.settleCalls().length, 0);
  } finally {
    await rig.close();
  }
});

test('an intent offer is paid on the payer terms: one creation, bound-checked, settled by the execution', async () => {
  const asked: IntentTermsRequest[] = [];
  const approvals: PaymentApproval[] = [];
  const rig = await startRig({
    offers: [INTENT],
    capabilities: { ...HOLDS, maxAmount: { KAS: SPEND_CAPS.KAS } },
    settle: (_n, fr) => ({ body: intentSettlement(fr) }),
    paywall: { maxTimeoutSeconds: 120 },
    client: {
      intentTerms: (q) => (asked.push(q), { maxSell: '3000', expiresInMs: 600_000 }),
      approve: (a) => (approvals.push(a), true),
    },
  });
  try {
    const { response, payment } = await rig.client.paidFetch(`${rig.base}/report`);
    assert.equal(response.status, 200, await response.clone().text());
    assert.equal(rig.handled.length, 1);
    assert.equal(asked.length, 1);
    assert.deepEqual([asked[0]!.payAsset, asked[0]!.bound], [TOKEN_A, SWAP_BOUNDS[TOKEN_A]]);
    const build = rig.wasm.calls.filter((c) => c.method === 'payIntent');
    assert.equal(build.length, 1, 'one creation');
    const req = build[0]!.arg as { payAsset: string; options: { maxSell: string; expiresInMs: number }; requestHash: string };
    assert.equal(req.payAsset, TOKEN_A);
    assert.equal(req.options.maxSell, '3000');
    assert.equal(req.options.expiresInMs, 120_000, "the intent's lifetime is capped by the offer's maxTimeoutSeconds");
    // preflighted against the bound of the pay asset, in its own units
    const pf = rig.wasm.calls.find((c) => c.method === 'preflight')!.arg as { maxPay: string; maxPayAsset: string };
    assert.deepEqual([pf.maxPay, pf.maxPayAsset], [SWAP_BOUNDS[TOKEN_A], TOKEN_A]);
    // approve sees the worst case (no ceiling for the merchant asset KAS beyond its amount: asked)
    assert.equal(approvals.length, 0, 'KAS has an explicit ceiling: nothing to ask');
    assert.ok(payment);
    assert.equal(payment.kind, 'intent');
    assert.equal(payment.settledTransaction, 'e0'.repeat(32), 'the execution paid the merchant');
    assert.notEqual(payment.transactionId, payment.settledTransaction);
    assert.ok(payment.intent, 'the intent handle comes back');
    const rec = (await rig.store.load(payment.paymentId))!;
    assert.equal(rec.status, 'settled');
    assert.equal(rec.kind, 'intent');
    assert.deepEqual(rec.intent, payment.intent, 'the handle was stored with the signed creation');
    assert.equal(rec.anchor, undefined, 'an intent creation has no anchor: it is never rebuilt');
  } finally {
    await rig.close();
  }
});

test('approve is shown the worst case of an intent when no ceiling covers it', async () => {
  const approvals: PaymentApproval[] = [];
  const rig = await startRig({
    offers: [INTENT],
    capabilities: { ...HOLDS, maxAmount: {} },
    settle: (_n, fr) => ({ body: intentSettlement(fr) }),
    client: { intentTerms: () => ({ maxSell: '2500' }), approve: (a) => (approvals.push(a), true) },
  });
  try {
    assert.equal((await rig.client.fetch(`${rig.base}/report`)).status, 200);
    assert.equal(approvals.length, 1);
    assert.deepEqual([approvals[0]!.offer.kind, approvals[0]!.cost.payAsset, approvals[0]!.cost.payerSpent], ['intent', TOKEN_A, '2500']);
  } finally {
    await rig.close();
  }
});

test('an intent whose worst case is above the maxPay bound is never stored or sent', async () => {
  const rig = await startRig({ offers: [INTENT], capabilities: HOLDS, client: { maxPay: { [TOKEN_A]: '2999' }, intentTerms: () => ({ maxSell: '3000' }) } });
  try {
    await assert.rejects(rig.client.fetch(`${rig.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'preflight_failed' && e.diagnostic === 'overpayment');
    assert.equal((await rig.store.list()).length, 0);
    assert.equal(rig.facilitator.settleCalls().length, 0);
    // terms the hook declines: not paid
    const declined = await startRig({ offers: [INTENT], capabilities: HOLDS, client: { intentTerms: () => undefined } });
    try {
      await assert.rejects(declined.client.fetch(`${declined.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'spend_not_authorized');
      assert.equal(declined.wasm.calls.filter((c) => c.method === 'payIntent').length, 0);
    } finally {
      await declined.close();
    }
  } finally {
    await rig.close();
  }
});

test('a creation is re-sent while it may still execute, never re-signed; a dead intent stops with its handle', async () => {
  // the keeper cannot execute yet, then the execution is pending, then it settles: the same creation every time
  const rig = await startRig({
    offers: [INTENT],
    capabilities: HOLDS,
    settle: (n, fr) => ({ body: n === 1 ? failure('intent_not_executable', true, 'no order fits yet') : n === 2 ? failure('settlement_pending', true, 'executing') : intentSettlement(fr) }),
    client: { intentTerms: () => ({ maxSell: '3000' }), retry: FAST },
  });
  try {
    const { response, payment } = await rig.client.paidFetch(`${rig.base}/report`);
    assert.equal(response.status, 200);
    assert.equal(rig.wasm.calls.filter((c) => c.method === 'payIntent').length, 1, 'signed once');
    const txs = rig.facilitator.settleCalls().map((c) => (c.body as FacilitatorRequest).paymentPayload.payload.transaction);
    assert.equal(txs.length, 3);
    assert.ok(txs.every((t) => t === txs[0]), 'the same creation every time');
    assert.deepEqual([payment!.attempts, payment!.resends], [1, 2]);
  } finally {
    await rig.close();
  }
  // a failure that would need a new signature (the inputs are gone, an order lost) stops: nothing is re-signed
  const dead = await startRig({
    offers: [INTENT],
    capabilities: HOLDS,
    settle: () => ({ body: failure('invalid_kaspa_exact_utxo', false, 'an input is spent') }),
    client: { intentTerms: () => ({ maxSell: '3000' }), retry: FAST },
  });
  try {
    await assert.rejects(dead.client.fetch(`${dead.base}/report`), (e: unknown) => {
      assert.ok(e instanceof KobX402Error);
      assert.equal(e.code, 'payment_failed');
      assert.equal(e.diagnostic, 'invalid_kaspa_exact_utxo');
      assert.ok(e.intent, 'the intent handle is on the error');
      return true;
    });
    assert.equal(dead.wasm.calls.filter((c) => c.method === 'payIntent').length, 1, 'never re-signed');
    assert.equal(dead.facilitator.settleCalls().length, 1);
    const [rec] = await dead.store.list();
    assert.equal(rec!.status, 'rejected');
    assert.ok(rec!.intent, 'the stored artifact keeps the handle for cancelIntent');
  } finally {
    await dead.close();
  }
});

test('a PAYMENT-RESPONSE that does not name the signed creation is refused', async () => {
  // a rogue merchant answers success for some other intent
  let challenge: PaymentRequired | undefined;
  const rig = await startRig({ offers: [INTENT] });
  try {
    challenge = (await (await fetch(`${rig.base}/report`)).json()) as PaymentRequired;
  } finally {
    await rig.close();
  }
  const raw = await startRaw((req, res) => {
    if (!req.headers['payment-signature']) {
      // the replayed challenge names this server's resource (a 402 for another resource is refused before anything is signed)
      const here = { ...challenge!, resource: { ...challenge!.resource, url: `http://${req.headers.host}${req.url}` } };
      res.writeHead(402, { 'content-type': 'application/json', [HEADER_PAYMENT_REQUIRED]: Buffer.from(JSON.stringify(here)).toString('base64') }).end(JSON.stringify(here));
      return;
    }
    const bogus: SettlementResponse = { success: true, transaction: 'e0'.repeat(32), network: NETWORK, amount: INTENT.amount, extensions: { kob: { intent: { creation: 'c1'.repeat(32) } } } };
    res.writeHead(200, { 'payment-response': encodePaymentResponse(bogus) }).end('paid?');
  });
  const wasm = stubWasm();
  const client = new KobX402Client({
    wasm,
    network: NETWORK,
    payerAddress: PAYER,
    privateKeys: ['01'.repeat(32)],
    context: staticContext(),
    store: new MemoryArtifactStore(),
    capabilities: { ...HOLDS, maxAmount: SPEND_CAPS },
    maxPay: SWAP_BOUNDS,
    intentTerms: () => ({ maxSell: '3000' }),
    retry: false,
    policy: completePolicy(raw.base),
  });
  try {
    await assert.rejects(client.fetch(`${raw.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'invalid_settlement' && /creation/.test(e.message));
  } finally {
    await raw.close();
  }
  // and the check itself
  const s: SettlementResponse = { success: true, transaction: 'e0'.repeat(32), network: NETWORK, amount: '5', extensions: { kob: { intent: { creation: 'c0'.repeat(32) } } } };
  verifySettlement(s, '5', NETWORK, 'C0'.repeat(32), null, true);
  assert.throws(() => verifySettlement(s, '5', NETWORK, 'c1'.repeat(32), null, true), /creation/);
  assert.throws(() => verifySettlement(s, '5', NETWORK, 'c0'.repeat(32)), /differs from the signed transaction/, 'a direct payment names its own transaction');
});

test('wallet flow: the wallet signs the creation once', async () => {
  const signed: unknown[] = [];
  const rig = await startRig({
    offers: [INTENT],
    capabilities: HOLDS,
    settle: (_n, fr) => ({ body: intentSettlement(fr) }),
    client: {
      privateKeys: undefined,
      wallet: { publicKey: '33'.repeat(32), signInputs: async (r) => (signed.push(r), r.map((x) => ({ inputIndex: x.inputIndex, signature: '44'.repeat(64) }))) },
      intentTerms: () => ({ maxSell: '3000' }),
    },
  });
  try {
    const { response, payment } = await rig.client.paidFetch(`${rig.base}/report`);
    assert.equal(response.status, 200);
    assert.equal(payment?.kind, 'intent');
    assert.equal(signed.length, 1);
    const prep = rig.wasm.calls.find((c) => c.method === 'prepareIntent')!.arg as { payerPublicKey: string; privateKeys?: string[] };
    assert.equal(prep.payerPublicKey, '33'.repeat(32));
    assert.equal(prep.privateKeys, undefined);
  } finally {
    await rig.close();
  }
});
