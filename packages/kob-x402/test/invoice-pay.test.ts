// KobX402Client.payInvoice: any entry of an invoice (native, kcc20, swap-and-pay, intent) is paid under the client's limits
// with the invoice id as the request hash, through the facilitator's `POST /invoices/<id>/pay`, retried without paying twice
// (re-send of an unknown outcome, anchored rebuild after a failure that released the inputs, never a second intent creation),
// and accepted only with a settlement that verifies (verifyInvoiceSettlement for a payment submitted by the caller).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { KobX402Client } from '../src/client.ts';
import type { KobX402ClientOptions } from '../src/client.ts';
import { MemoryArtifactStore } from '../src/artifact-store.ts';
import { InvoiceClient, invoiceId, newInvoice, verifyInvoiceSettlement } from '../src/invoice.ts';
import type { FetchedInvoice } from '../src/invoice.ts';
import { payInvoiceWithIntent } from '../src/intent.ts';
import { KobX402Error } from '../src/errors.ts';
import { ASSET_KAS, BINDING_EXACT, BINDING_INTENT, BINDING_SWAP, ROUTER_ARTIFACT_ID, TX_ENCODING } from '../src/types.ts';
import type { PaymentPayload, PaymentRequirements, SettlementResponse } from '../src/types.ts';
import { NETWORK, PAYER, SPEND_CAPS, SWAP_BOUNDS, TOKEN_A, TOKEN_B, completePolicy, staticContext } from './helpers/env.ts';
import { failure } from './helpers/stub-facilitator.ts';
import { stubWasm } from './helpers/stub-wasm.ts';

const BASE = 'https://pay.example';
const FAST = { baseDelayMs: 1, maxDelayMs: 2, sleep: async () => undefined };

function entry(amount: string, extra: Partial<PaymentRequirements['extra']> = {}, asset = ASSET_KAS): PaymentRequirements {
  return {
    scheme: 'exact',
    network: NETWORK,
    amount,
    asset,
    payTo: 'kaspatest:qmerchant',
    maxTimeoutSeconds: 600,
    extra: { binding: BINDING_EXACT, profile: 'standard-native', paymentFlow: 'upfront', finality: 'accepted', transactionEncoding: TX_ENCODING, payToScriptPublicKey: '0000', ...extra },
  };
}
const NATIVE = entry('100000000');
const KCC20 = entry(
  '700',
  { profile: 'kcc20', token: { family: 'kcc20', templateHash: '40'.repeat(32), extensionCommitment: 'ee'.repeat(32), custody: 'unconditional', carrier: '100000000', tokenScriptPublicKey: '0000aa' } },
  TOKEN_B,
);
const SWAP = entry('200000000', { route: { binding: BINDING_SWAP, critical: true, payAssets: [{ asset: TOKEN_A, templateHash: '40'.repeat(32), extensionCommitment: 'ee'.repeat(32) }] } });
const INTENT = entry('200000000', { route: { binding: BINDING_INTENT, critical: true, router: ROUTER_ARTIFACT_ID, payAssets: [{ asset: TOKEN_A, templateHash: '40'.repeat(32), extensionCommitment: 'ee'.repeat(32) }] } });

function fetched(accepts: PaymentRequirements[], expiresAtMs = Date.now() + 600_000): FetchedInvoice {
  const invoice = newInvoice({ network: NETWORK, reference: `order-${Math.random()}`, expiresAtMs, accepts });
  const id = invoiceId(invoice);
  return { invoice, id, payUrl: `${BASE}/invoices/${id}/pay`, statusUrl: `${BASE}/invoices/${id}/status`, kaspaUri: null, expiresAtMs };
}

/** The settlement a facilitator answers for a direct payment (its own transaction), with a kcc20 entry's custody class. */
function settled(p: PaymentPayload): SettlementResponse {
  const txid = (JSON.parse(p.payload.transaction) as { id: string }).id;
  const custody = (p.accepted.extra.token as { custody?: string } | undefined)?.custody;
  return { success: true, transaction: txid, network: p.accepted.network, amount: p.accepted.amount, extensions: { kaspa: { finality: 'accepted', ...(custody && !p.accepted.extra.route ? { custody } : {}) } } };
}

/** An intent settlement: the execution in `transaction`, the creation in `extensions.kob.intent.creation`. */
function executed(p: PaymentPayload): SettlementResponse {
  const s = settled(p);
  return { ...s, transaction: 'e0'.repeat(32), extensions: { ...s.extensions, kob: { intent: { creation: s.transaction, executions: 1 } } } };
}

type Answer = SettlementResponse | { status: number; body: unknown } | 'drop';

/** An InvoiceClient whose facilitator answers from a script (the last answer repeats). */
function facilitator(answers: ((p: PaymentPayload) => Answer)[]) {
  const posts: { url: string; payload: PaymentPayload }[] = [];
  const fetch = async (url: string, init?: RequestInit): Promise<Response> => {
    const payload = JSON.parse(String(init?.body)) as PaymentPayload;
    posts.push({ url, payload });
    const a = (answers[posts.length - 1] ?? answers[answers.length - 1]!)(payload);
    if (a === 'drop') throw new TypeError('fetch failed');
    if ('status' in a && typeof a.status === 'number' && 'body' in a) return new Response(JSON.stringify(a.body), { status: a.status, headers: { 'content-type': 'application/json' } });
    return new Response(JSON.stringify(a), { status: 200, headers: { 'content-type': 'application/json' } });
  };
  return { invoices: new InvoiceClient({ url: BASE, fetch }), posts };
}

function client(extra: Partial<KobX402ClientOptions> = {}) {
  const wasm = stubWasm();
  const store = new MemoryArtifactStore();
  const c = new KobX402Client({
    wasm,
    network: NETWORK,
    payerAddress: PAYER,
    privateKeys: ['01'.repeat(32)],
    context: staticContext(),
    store,
    capabilities: { maxAmount: SPEND_CAPS, tokens: { [TOKEN_A]: '100000', [TOKEN_B]: '100000' } },
    maxPay: SWAP_BOUNDS,
    retry: FAST,
    policy: completePolicy(BASE, 'kaspatest:qmerchant'),
    ...extra,
  });
  return { c, wasm, store };
}

test('a native entry is paid with the invoice id as the request hash, stored first and verified', async () => {
  const inv = fetched([KCC20, NATIVE]);
  const { invoices, posts } = facilitator([settled]);
  const { c, wasm, store } = client();
  const r = await c.payInvoice(inv, invoices);
  assert.equal(r.invoiceId, inv.id);
  assert.equal(r.acceptIndex, 1, 'native ranks first');
  assert.equal(r.kind, 'native');
  assert.equal(r.settledTransaction, r.transactionId);
  const built = wasm.calls.find((c) => c.method === 'payNative')!.arg as { requestHash: string };
  assert.equal(built.requestHash, inv.id);
  assert.equal(posts.length, 1);
  assert.equal(posts[0]!.url, `${BASE}/invoices/${inv.id}/pay`);
  assert.equal(posts[0]!.payload.payload.requestHash, inv.id);
  const rec = (await store.load(r.paymentId))!;
  assert.deepEqual([rec.status, rec.url, rec.method, rec.requestHash], ['settled', inv.payUrl, 'POST', inv.id]);
  // a second payInvoice of the paid invoice signs anew only when asked; here the earlier artifact is settled, so it may
  const again = facilitator([() => failure('invoice_paid', false, 'already paid')]);
  await assert.rejects(c.payInvoice(inv, again.invoices), (e: unknown) => e instanceof KobX402Error && e.code === 'payment_failed' && e.diagnostic === 'invoice_paid');
  assert.equal(again.posts.length, 1, 'a refusal is not re-sent');
});

test('a kcc20 entry needs a settlement carrying its custody class', async () => {
  const inv = fetched([NATIVE, KCC20]);
  const { invoices } = facilitator([(p) => ({ ...settled(p), extensions: { kaspa: { finality: 'accepted' } } })]);
  const { c, store } = client();
  await assert.rejects(c.payInvoice(inv, invoices, { acceptIndex: 1 }), (e: unknown) => e instanceof KobX402Error && e.code === 'invalid_settlement' && /custody/.test(e.message));
  const [rec] = await store.list();
  assert.equal(rec!.status, 'pending', 'a settlement that does not verify leaves the payment unreconciled');
  const ok = facilitator([settled]);
  const r = await client().c.payInvoice(inv, ok.invoices, { acceptIndex: 1 });
  assert.deepEqual([r.kind, r.acceptIndex], ['kcc20', 1]);
});

test('a swap entry whose order was taken is rebuilt, anchored, under a fresh payment id; settled once', async () => {
  const inv = fetched([SWAP]);
  const { invoices, posts } = facilitator([() => failure('order_conflict', true, 'an order was taken'), settled]);
  const { c, wasm, store } = client();
  const r = await c.payInvoice(inv, invoices);
  assert.deepEqual([r.kind, r.attempts, r.superseded.length], ['swap', 2, 1]);
  const builds = wasm.calls.filter((x) => x.method === 'paySwap').map((x) => x.arg as { requestHash: string; paymentId: string; maxPayAmount?: string });
  assert.equal(builds.length, 2);
  assert.ok(builds.every((b) => b.requestHash === inv.id && b.maxPayAmount === SWAP_BOUNDS[TOKEN_A]));
  assert.notEqual(builds[0]!.paymentId, builds[1]!.paymentId);
  assert.notEqual(posts[0]!.payload.payload.transaction, posts[1]!.payload.payload.transaction);
  const recs = await store.list();
  assert.deepEqual(recs.map((x) => x.status).sort(), ['settled', 'superseded']);
  assert.ok(recs.every((x) => x.anchor === recs[0]!.anchor && x.anchor !== undefined), 'every attempt spends the anchor');
});

test('an unknown outcome is re-sent with the same payment: no answer, a 503, invoice_pending, then the settlement', async () => {
  const inv = fetched([NATIVE]);
  const { invoices, posts } = facilitator([
    () => 'drop',
    () => ({ status: 503, body: { error: 'busy', extensions: { kaspa: { diagnostic: 'rate_limited', retryable: true, message: 'busy' } } } }),
    () => failure('invoice_pending', true, 'another payment is being settled'),
    settled,
  ]);
  const { c, wasm } = client();
  const r = await c.payInvoice(inv, invoices);
  assert.deepEqual([r.attempts, r.resends], [1, 3]);
  assert.equal(wasm.calls.filter((x) => x.method === 'payNative').length, 1, 'signed once');
  assert.ok(posts.every((p) => p.payload.payload.transaction === posts[0]!.payload.payload.transaction));
});

test('a settlement for another transaction is refused; an expired or foreign invoice is never built', async () => {
  const inv = fetched([NATIVE]);
  const { invoices } = facilitator([(p) => ({ ...settled(p), transaction: 'ff'.repeat(32) })]);
  const { c } = client({ retry: false });
  await assert.rejects(c.payInvoice(inv, invoices), (e: unknown) => e instanceof KobX402Error && e.code === 'invalid_settlement');
  const { c: c2, wasm } = client();
  await assert.rejects(c2.payInvoice(fetched([NATIVE], Date.now() - 1), invoices), (e: unknown) => e instanceof KobX402Error && e.diagnostic === 'invoice_expired');
  const foreign = fetched([NATIVE]);
  foreign.invoice = { ...foreign.invoice, network: 'kaspa:mainnet' };
  await assert.rejects(c2.payInvoice(foreign, invoices), (e: unknown) => e instanceof KobX402Error && e.code === 'no_acceptable_offer');
  assert.equal(wasm.calls.filter((x) => x.method.startsWith('pay')).length, 0);
});

test('an intent entry: one creation on intentTerms, its lifetime within the invoice; a pending execution ends payment_pending with the handle', async () => {
  const inv = fetched([INTENT], Date.now() + 30_000);
  const { invoices, posts } = facilitator([() => failure('settlement_pending', true, 'executing')]);
  const { c, wasm, store } = client({ intentTerms: () => ({ maxSell: '3000', expiresInMs: 600_000 }), retry: { ...FAST, resends: 2 } });
  await assert.rejects(c.payInvoice(inv, invoices), (e: unknown) => {
    assert.ok(e instanceof KobX402Error);
    assert.equal(e.code, 'payment_pending');
    assert.equal(e.diagnostic, 'settlement_pending');
    assert.ok(e.intent, 'the intent handle');
    return true;
  });
  const built = wasm.calls.filter((x) => x.method === 'payIntent');
  assert.equal(built.length, 1, 'one creation, never re-signed');
  const life = (built[0]!.arg as { options: { expiresInMs: number }; requestHash: string }).options.expiresInMs;
  assert.ok(life <= 30_000 && life > 25_000, `the intent lives at most until the invoice expires (${life})`);
  assert.equal(posts.length, 3, 'sent once, re-sent twice');
  const [rec] = await store.list();
  assert.deepEqual([rec!.status, rec!.kind], ['pending', 'intent']);
  assert.ok(rec!.intent);
  // executed: the settlement names the creation
  const ok = facilitator([() => failure('settlement_pending', true, 'executing'), executed]);
  const r = await client({ intentTerms: () => ({ maxSell: '3000' }) }).c.payInvoice(fetched([INTENT]), ok.invoices);
  assert.deepEqual([r.kind, r.settledTransaction, r.resends], ['intent', 'e0'.repeat(32), 1]);
  assert.ok(r.intent);
});

test('verifyInvoiceSettlement, and payInvoiceWithIntent refuses a success that is not its creation', async () => {
  const inv = fetched([NATIVE, INTENT]);
  const payload = { x402Version: 2, accepted: INTENT, payload: { requestHash: inv.id, transaction: '{}' } } as unknown as PaymentPayload;
  const good: SettlementResponse = { success: true, transaction: 'e0'.repeat(32), network: NETWORK, amount: INTENT.amount, extensions: { kob: { intent: { creation: 'cd'.repeat(32) } } } };
  verifyInvoiceSettlement(good, inv, { paymentPayload: payload, transactionId: 'cd'.repeat(32) });
  assert.throws(() => verifyInvoiceSettlement(good, inv, { paymentPayload: payload, transactionId: 'ce'.repeat(32) }), /creation/);
  assert.throws(() => verifyInvoiceSettlement({ ...good, amount: '1' }, inv, { paymentPayload: payload, transactionId: 'cd'.repeat(32) }), /amount/);
  assert.throws(() => verifyInvoiceSettlement(good, inv, { paymentPayload: { ...payload, payload: { ...payload.payload, requestHash: 'ab'.repeat(32) } }, transactionId: 'cd'.repeat(32) }), /request hash/);
  assert.throws(() => verifyInvoiceSettlement(good, inv, { paymentPayload: { ...payload, accepted: entry('5') }, transactionId: 'cd'.repeat(32) }), /entries/);

  const wasm = {
    payIntent: (r: { requirements: PaymentRequirements; requestHash: string }) => ({
      paymentPayload: { x402Version: 2, accepted: r.requirements, payload: { requestHash: r.requestHash } },
      transactionId: 'cd'.repeat(32),
      consumed: [],
      feeSompi: '1',
      expiresAtMs: Date.now() + 300_000,
      payerSpent: '3000',
      intent: { actor: 'TokenToKas_sell', state: {}, intent: {} },
    }),
  } as never;
  const rogue = facilitator([() => ({ ...good, extensions: { kob: { intent: { creation: 'c1'.repeat(32) } } } })]);
  await assert.rejects(payInvoiceWithIntent(wasm, rogue.invoices, inv, { payAsset: TOKEN_A, utxos: [], nowMs: Date.now(), options: { maxSell: '3000' } }, { retry: false }), (e: unknown) => {
    assert.ok(e instanceof KobX402Error);
    assert.equal(e.code, 'invalid_settlement');
    assert.ok(e.payment?.intent, 'the signed creation comes back to cancel it');
    return true;
  });
  const honest = facilitator([() => good]);
  const r = await payInvoiceWithIntent(wasm, honest.invoices, inv, { payAsset: TOKEN_A, utxos: [], nowMs: Date.now(), options: { maxSell: '3000' } });
  assert.equal(r.settlement.success, true);
});
