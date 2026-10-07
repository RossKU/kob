// Paywall + client end to end over real HTTP against an in-process stub facilitator and a stub KobWasm.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { decodePaymentRequired, decodePaymentResponse, encodePaymentSignature } from '../src/headers.ts';
import { KobX402Error } from '../src/errors.ts';
import { canonicalJson, httpRequestHash, requirementsHash } from '../src/canonical.ts';
import { MemoryArtifactStore } from '../src/artifact-store.ts';
import type { ArtifactRecord } from '../src/artifact-store.ts';
import type { OfferSpec, PaymentRequired } from '../src/types.ts';
import { KobX402Client } from '../src/client.ts';
import type { KobX402ClientOptions } from '../src/client.ts';
import { NATIVE_OFFER, NETWORK, PAYER, SPEND_CAPS, TOKEN_A, TOKEN_B, staticContext, startRig } from './helpers/env.ts';
import { stubWasm } from './helpers/stub-wasm.ts';
import { defaultSettlement, failure } from './helpers/stub-facilitator.ts';

test('unpaid request: 402 with PAYMENT-REQUIRED (header and body agree) advertising payment-identifier', async () => {
  const rig = await startRig({ offers: [NATIVE_OFFER, { kind: 'kcc20', asset: TOKEN_B, amount: '700', token: { custody: 'unconditional' } }] });
  try {
    const res = await fetch(`${rig.base}/report`);
    assert.equal(res.status, 402);
    const header = decodePaymentRequired(res.headers.get('payment-required')!);
    const body = (await res.json()) as PaymentRequired;
    assert.deepEqual(body, header);
    assert.equal(header.x402Version, 2);
    assert.equal(header.resource.url, `${rig.base}/report`);
    assert.equal(header.accepts.length, 2);
    assert.equal(header.accepts[0]!.extra.profile, 'standard-native');
    assert.equal(header.accepts[1]!.extra.profile, 'kcc20');
    assert.deepEqual(header.extensions?.['payment-identifier']?.info, { required: true });
    assert.equal(rig.facilitator.settleCalls().length, 0);
    assert.equal(rig.handled.length, 0);
  } finally {
    await rig.close();
  }
});

test('happy path (KAS): pays, records the artifact, gets the resource and a verified PAYMENT-RESPONSE', async () => {
  const rig = await startRig({ apiKey: 'sekret' });
  try {
    const { response, payment } = await rig.client.paidFetch(`${rig.base}/report?a=1`);
    assert.equal(response.status, 200);
    assert.equal(await response.text(), 'report for GET /report');
    assert.ok(payment);
    assert.equal(payment.kind, 'native');
    assert.equal(payment.amount, '50000000');
    const settlement = decodePaymentResponse(response.headers.get('payment-response')!);
    assert.equal(settlement.success, true);
    assert.equal(settlement.transaction, payment.transactionId);

    // the facilitator got the paid payload, our offer, the independently computed requestHash, and the API key
    const settle = rig.facilitator.settleCalls();
    assert.equal(settle.length, 1);
    assert.equal(settle[0]!.headers.authorization, 'Bearer sekret');
    const fr = settle[0]!.body as any;
    const expectedHash = httpRequestHash('GET', `${rig.base}/report?a=1`, null, requirementsHash(fr.paymentRequirements));
    assert.equal(fr.requestHash, expectedHash);
    assert.equal(fr.paymentPayload.payload.requestHash, expectedHash);
    assert.equal(canonicalJson(fr.paymentPayload.accepted), canonicalJson(fr.paymentRequirements));
    assert.match(fr.paymentPayload.extensions['payment-identifier'].info.id, /^[A-Za-z0-9_-]{16,128}$/);
    assert.equal(fr.resource.url, `${rig.base}/report?a=1`);

    // the artifact is stored and settled
    const rec = (await rig.store.list())[0]!;
    assert.equal(rec.status, 'settled');
    assert.equal(rec.transactionId, payment.transactionId);
    assert.equal(rec.paymentPayload.payload.transaction, fr.paymentPayload.payload.transaction);
    // preflight ran before disclosure
    const order = rig.wasm.calls.map((c) => c.method).filter((m) => m === 'payNative' || m === 'preflight');
    assert.deepEqual(order, ['payNative', 'preflight']);
  } finally {
    await rig.close();
  }
});

test('POST with a JSON body binds the body into the request hash on both sides', async () => {
  const rig = await startRig();
  try {
    const res = await rig.client.fetch(`${rig.base}/report`, { method: 'POST', body: '{"z":1,"a":[2]}', headers: { 'content-type': 'application/json' } });
    assert.equal(res.status, 200);
    const fr = rig.facilitator.settleCalls()[0]!.body as any;
    const expected = httpRequestHash('POST', `${rig.base}/report`, { a: [2], z: 1 }, requirementsHash(fr.paymentRequirements));
    assert.equal(fr.requestHash, expected);
    assert.equal(rig.handled[0]!.body, '{"z":1,"a":[2]}');
  } finally {
    await rig.close();
  }
});

test('kcc20 and swap-and-pay entries go through the matching KobWasm builders', async () => {
  const specs: OfferSpec[][] = [
    [{ kind: 'kcc20', asset: TOKEN_B, amount: '700', token: { custody: 'unconditional' } }],
    [{ kind: 'swap', receive: 'kas', amount: '50000000', payAssets: [{ asset: TOKEN_A }] }],
  ];
  const want = ['payKcc20', 'paySwap'];
  for (const [i, offers] of specs.entries()) {
    const rig = await startRig({ offers, capabilities: { tokens: { [TOKEN_A]: '99', [TOKEN_B]: '1000' } } });
    try {
      const { response, payment } = await rig.client.paidFetch(`${rig.base}/report`);
      assert.equal(response.status, 200);
      assert.equal(payment?.kind, i === 0 ? 'kcc20' : 'swap');
      assert.ok(rig.wasm.calls.some((c) => c.method === want[i]));
      const fr = rig.facilitator.settleCalls()[0]!.body as any;
      assert.equal(fr.paymentPayload.payload.authorization.version, 'kob-x402-payload-commitment-v1');
      if (i === 1) assert.equal(fr.paymentPayload.payload.route.payAsset, TOKEN_A);
    } finally {
      await rig.close();
    }
  }
});

test('order_conflict: the client surfaces the retryable failure; a second call revokes the first artifact (allowResign), re-quotes, re-signs and succeeds', async () => {
  const conflicts = [{ txid: '99'.repeat(32), index: 0 }];
  const rig = await startRig({
    settle: (n) => (n === 1 ? { body: failure('order_conflict', true, 'an order was consumed', conflicts) } : undefined),
    client: { allowResign: () => true, submit: async (tx) => (JSON.parse(tx) as { id: string }).id },
  });
  try {
    let err: unknown;
    try {
      await rig.client.fetch(`${rig.base}/report`);
    } catch (e) {
      err = e;
    }
    assert.ok(err instanceof KobX402Error);
    assert.equal(err.code, 'payment_failed');
    assert.equal(err.diagnostic, 'order_conflict');
    assert.equal(err.retryable, true);
    assert.deepEqual(err.details, conflicts);
    assert.equal(err.status, 402);
    assert.equal(rig.facilitator.settleCalls().length, 1, 'no automatic re-send of the payment');
    const first = (await rig.store.list())[0]!;
    assert.equal(first.status, 'rejected');
    assert.equal(first.failure?.diagnostic, 'order_conflict');

    const { response, payment } = await rig.client.paidFetch(`${rig.base}/report`);
    assert.equal(response.status, 200);
    assert.equal(rig.facilitator.settleCalls().length, 2);
    assert.notEqual(payment!.paymentId, first.paymentId);
    assert.notEqual(payment!.transactionId, first.transactionId, 're-signed: a different transaction');
    assert.equal(rig.context.loads, 2, 'a fresh quote loads a fresh chain context');
    const all = await rig.store.list();
    assert.deepEqual(all.map((r) => r.status), ['revoked', 'settled'], 'the first artifact is revoked before the second is signed');
    assert.equal(rig.wasm.calls.filter((c) => c.method === 'revoke').length, 1);
  } finally {
    await rig.close();
  }
});

test('a retryable 402 never makes the client silently sign a second payment (no allowResign policy)', async () => {
  const rig = await startRig({ settle: (n) => (n === 1 ? { body: failure('order_conflict', true, 'an order was consumed') } : undefined) });
  try {
    await assert.rejects(rig.client.fetch(`${rig.base}/report`), (e: unknown) => e instanceof KobX402Error && e.retryable === true);
    const signed = () => rig.wasm.calls.filter((c) => c.method === 'payNative').length;
    assert.equal(signed(), 1);
    await assert.rejects(rig.client.fetch(`${rig.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'payment_in_flight');
    assert.equal(signed(), 1, 'nothing was signed the second time');
    assert.equal(rig.facilitator.settleCalls().length, 1);
    assert.equal((await rig.store.list()).length, 1);
    // an allowResign policy without a way to submit the revoke does not help either: the revoke must be submitted first
    const c2 = new KobX402Client({ wasm: rig.wasm, network: NETWORK, payerAddress: PAYER, privateKeys: ['01'.repeat(32)], context: rig.context, store: rig.store, capabilities: { maxAmount: SPEND_CAPS }, allowResign: () => true });
    await assert.rejects(c2.fetch(`${rig.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'payment_in_flight');
    assert.equal(signed(), 1);
    // a revoked artifact frees the resource
    await rig.client.revoke((await rig.store.list())[0]!.paymentId).catch(() => undefined);
  } finally {
    await rig.close();
  }
});

test('non-retryable failures surface retryable=false; unauthorized facilitator is hidden behind a 502', async () => {
  const rig = await startRig({ settle: () => ({ body: failure('underpayment', false, 'too little', undefined, 'invalid_payload') }) });
  try {
    await assert.rejects(rig.client.fetch(`${rig.base}/report`), (e: unknown) => e instanceof KobX402Error && e.diagnostic === 'underpayment' && e.retryable === false);
  } finally {
    await rig.close();
  }
  const rig2 = await startRig({ settle: () => ({ status: 401, body: failure('unauthorized', false, 'bad key') }) });
  try {
    const res = await fetch(`${rig2.base}/report`, { headers: { 'payment-signature': 'x' } });
    assert.equal(res.status, 402, 'garbage signature: corrective 402');
    await assert.rejects(rig2.client.fetch(`${rig2.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'payment_pending' && e.status === 502);
  } finally {
    await rig2.close();
  }
});

test('crash recovery: resume() re-sends the STORED artifact; idempotency replays the settlement, the facilitator settles once', async () => {
  let dropPaidResponse = true;
  const lossy = async (u: string, init?: RequestInit): Promise<Response> => {
    const r = await fetch(u, init);
    if (dropPaidResponse && new Headers(init?.headers).has('payment-signature')) throw new TypeError('connection reset after the merchant processed the payment');
    return r;
  };
  const rig = await startRig({ client: { fetch: lossy } });
  try {
    const id = 'idempotent-payment-id-0001';
    const url = `${rig.base}/report`;
    await assert.rejects(rig.client.paidFetch(url, { paymentId: id }), (e: unknown) => e instanceof KobX402Error && e.code === 'payment_pending' && e.paymentId === id);
    const stored = (await rig.store.load(id))!;
    assert.equal(stored.status, 'pending');
    assert.equal(rig.facilitator.settleCalls().length, 1, 'the merchant did settle');

    dropPaidResponse = false;
    // a different request cannot be paired with the artifact
    await assert.rejects(rig.client.resume(id, { method: 'POST', body: 'x' }), (e: unknown) => e instanceof KobX402Error && e.code === 'bad_request');
    const { response, payment } = await rig.client.resume(id);
    assert.equal(response.status, 200);
    assert.equal(payment!.transactionId, stored.transactionId, 'the very transaction that was signed');
    assert.equal(rig.facilitator.settleCalls().length, 1, 'not settled twice');
    assert.deepEqual(rig.handled.map((h) => h.replayed), [false, true]);
    assert.equal((await rig.store.load(id))!.status, 'settled');
    await assert.rejects(rig.client.resume(id), (e: unknown) => e instanceof KobX402Error && e.code === 'bad_request', 'settled artifacts are not re-sent');
  } finally {
    await rig.close();
  }
});

/** Options of a second payer with the same wallet and chain view but its own artifact store. */
function otherClientOptions(rig: Awaited<ReturnType<typeof startRig>>): KobX402ClientOptions {
  return {
    wasm: rig.wasm,
    network: NETWORK,
    payerAddress: PAYER,
    privateKeys: ['01'.repeat(32)],
    context: rig.context,
    capabilities: { maxAmount: SPEND_CAPS },
    store: new MemoryArtifactStore(),
  };
}

test('idempotency: re-SIGNING under a settled payment id is refused by the merchant (409), nothing is settled twice', async () => {
  const rig = await startRig();
  try {
    const id = 'idempotent-payment-id-0003';
    const url = `${rig.base}/report`;
    await rig.client.fetch(url, { paymentId: id });
    // the same client refuses first: its store holds the artifact of the first transaction and never overwrites it
    await assert.rejects(rig.client.fetch(url, { paymentId: id }), (e: unknown) => e instanceof KobX402Error && e.code === 'artifact_store');
    assert.equal(rig.facilitator.settleCalls().length, 1);
    // a client with another store re-signs: another transaction under a settled id is not the settled payment
    const other = new KobX402Client({ ...otherClientOptions(rig) });
    await assert.rejects(other.fetch(url, { paymentId: id }), (e: unknown) => e instanceof KobX402Error && e.code === 'payment_pending' && e.status === 409);
    assert.equal(rig.handled.length, 1, 'served once');
    assert.equal(rig.facilitator.settleCalls().length, 1);
  } finally {
    await rig.close();
  }
});

test('idempotency: the same payment id for a different request is a conflict (409), nothing is settled twice', async () => {
  const rig = await startRig();
  try {
    const id = 'idempotent-payment-id-0002';
    await rig.client.fetch(`${rig.base}/report`, { paymentId: id });
    // a second paid request that reuses the id with a different URL (from a client with its own store: the same store refuses it)
    await assert.rejects(rig.client.paidFetch(`${rig.base}/other`, { paymentId: id }), (e: unknown) => e instanceof KobX402Error && e.code === 'artifact_store');
    const res = await new KobX402Client({ ...otherClientOptions(rig) }).paidFetch(`${rig.base}/other`, { paymentId: id }).then(
      (r) => r.response.status,
      (e: KobX402Error) => e.status,
    );
    assert.equal(res, 409);
    assert.equal(rig.facilitator.settleCalls().length, 1);
  } finally {
    await rig.close();
  }
});

test('the paywall recomputes: a payload with a foreign requestHash is refused before the facilitator is called', async () => {
  const rig = await startRig();
  try {
    const url = `${rig.base}/report`;
    const challenge = await fetch(url);
    const pr = decodePaymentRequired(challenge.headers.get('payment-required')!);
    // sign for a DIFFERENT request (other URL) and replay that artifact against /report
    const other = await rig.client.paidFetch(`${rig.base}/somewhere-else`, { paymentId: 'other-request-payment-01' });
    const stolen = (await rig.store.load('other-request-payment-01'))!.paymentPayload;
    assert.equal(other.response.status, 200);
    assert.equal(stolen.accepted.amount, pr.accepts[0]!.amount);
    const before = rig.facilitator.settleCalls().length;
    const res = await fetch(url, { headers: { 'payment-signature': encodePaymentSignature(stolen) } });
    assert.equal(res.status, 402);
    const corrective = decodePaymentRequired(res.headers.get('payment-required')!);
    assert.equal(corrective.extensions?.kaspa?.diagnostic, 'invalid_kaspa_x402_payload');
    assert.match(corrective.extensions?.kaspa?.message ?? '', /requestHash/);
    assert.equal(corrective.error, 'invalid_payload');
    assert.equal(rig.facilitator.settleCalls().length, before, 'the facilitator was not called');
  } finally {
    await rig.close();
  }
});

test('the paywall refuses a payload whose accepted differs from its offers, or with no / bad payment identifier', async () => {
  const rig = await startRig();
  try {
    const url = `${rig.base}/report`;
    await rig.client.fetch(url, { paymentId: 'base-payment-for-cloning-01' });
    const good = (await rig.store.load('base-payment-for-cloning-01'))!.paymentPayload;
    const cases: [string, (p: typeof good) => void, string][] = [
      ['accepted amount lowered', (p) => (p.accepted.amount = '1'), 'invalid_kaspa_x402_accepted'],
      ['accepted payTo swapped', (p) => (p.accepted.payTo = 'kaspatest:qqevil'), 'invalid_kaspa_x402_accepted'],
      ['no extensions', (p) => delete p.extensions, 'missing_kaspa_payment_identifier'],
      ['id absent', (p) => (p.extensions = { 'payment-identifier': { info: { required: true } } }), 'missing_kaspa_payment_identifier'],
      ['id too short', (p) => (p.extensions = { 'payment-identifier': { info: { required: true, id: 'short' } } }), 'invalid_kaspa_payment_identifier'],
      ['id with bad characters', (p) => (p.extensions = { 'payment-identifier': { info: { required: true, id: 'bad id with spaces!!' } } }), 'invalid_kaspa_payment_identifier'],
      ['wrong x402 version', (p) => (p.x402Version = 1), 'invalid_kaspa_x402_payload'],
      ['wrong payload type', (p) => (p.payload.type = 'other'), 'invalid_kaspa_x402_payload'],
    ];
    const before = rig.facilitator.settleCalls().length;
    for (const [name, mutate, diag] of cases) {
      const p = structuredClone(good);
      mutate(p);
      const res = await fetch(url, { headers: { 'payment-signature': encodePaymentSignature(p) } });
      assert.equal(res.status, 402, name);
      assert.equal(decodePaymentRequired(res.headers.get('payment-required')!).extensions?.kaspa?.diagnostic, diag, name);
    }
    assert.equal(rig.facilitator.settleCalls().length, before);
  } finally {
    await rig.close();
  }
});

test('missing payment identifier from a client that omits it: corrective 402 names the diagnostic', async () => {
  const rig = await startRig();
  try {
    await rig.client.fetch(`${rig.base}/report`, { paymentId: 'clone-source-payment-0001' });
    const p = structuredClone((await rig.store.load('clone-source-payment-0001'))!.paymentPayload);
    delete p.extensions;
    const res = await fetch(`${rig.base}/report`, { headers: { 'payment-signature': encodePaymentSignature(p) } });
    assert.equal(res.status, 402);
    const body = (await res.json()) as PaymentRequired;
    assert.equal(body.extensions?.kaspa?.diagnostic, 'missing_kaspa_payment_identifier');
    assert.equal(body.extensions?.kaspa?.retryable, false);
    assert.ok(body.accepts.length > 0, 'a corrective 402 carries the offers again');
    assert.deepEqual(body.extensions?.['payment-identifier']?.info, { required: true });
  } finally {
    await rig.close();
  }
});

test('the paywall fails closed on a success settlement that does not match its offer', async () => {
  for (const mutate of [
    (s: any) => (s.amount = '1'),
    (s: any) => (s.network = 'kaspa:mainnet'),
    (s: any) => (s.transaction = 'not-a-txid'),
  ]) {
    const rig = await startRig({
      settle: (_n, body) => {
        const s = defaultSettlement(body) as any;
        mutate(s);
        return { body: s };
      },
    });
    try {
      const res = await fetch(`${rig.base}/report`);
      assert.equal(res.status, 402);
      await assert.rejects(rig.client.fetch(`${rig.base}/report`), (e: unknown) => e instanceof KobX402Error && e.status === 502);
      assert.equal(rig.handled.length, 0, 'the resource is never served');
    } finally {
      await rig.close();
    }
  }
});

test('a facilitator that does not answer yields 503 and the client keeps the artifact pending', async () => {
  const rig = await startRig({ settle: () => ({ drop: true }) });
  try {
    await assert.rejects(rig.client.fetch(`${rig.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'payment_pending' && e.status === 503);
    const rec = (await rig.store.list())[0]!;
    assert.equal(rec.status, 'pending');
    assert.equal(rig.handled.length, 0);
  } finally {
    await rig.close();
  }
});

test('durability: the artifact is stored BEFORE the paid retry is sent', async () => {
  const events: string[] = [];
  const store = new MemoryArtifactStore();
  const save = store.save.bind(store);
  store.save = async (r: ArtifactRecord) => {
    events.push('save:' + r.status);
    await save(r);
  };
  const rig = await startRig({ store, paywall: { handler: () => new Response('ok') } });
  rig.server.on('request', (req) => events.push(req.headers['payment-signature'] ? 'request:paid' : 'request:unpaid'));
  try {
    await rig.client.fetch(`${rig.base}/report`);
    assert.deepEqual(events, ['request:unpaid', 'save:signed', 'request:paid']);
  } finally {
    await rig.close();
  }
});

test('durability: if the artifact cannot be stored nothing is disclosed', async () => {
  const store = new MemoryArtifactStore();
  store.save = async () => {
    throw new KobX402Error('artifact_store', 'disk full');
  };
  const rig = await startRig({ store });
  try {
    await assert.rejects(rig.client.fetch(`${rig.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'artifact_store');
    assert.equal(rig.facilitator.settleCalls().length, 0);
  } finally {
    await rig.close();
  }
});

test('builder and preflight problems abort before anything is stored or sent', async () => {
  const rig = await startRig();
  try {
    rig.wasm.preflightResult = { ok: false, diagnostic: 'underpayment', message: 'merchant would receive less' };
    await assert.rejects(rig.client.fetch(`${rig.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'preflight_failed' && e.diagnostic === 'underpayment');
    rig.wasm.preflightResult = { ok: true };
    rig.wasm.failWith = 'insufficient funds';
    await assert.rejects(rig.client.fetch(`${rig.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'payment_failed');
    rig.wasm.failWith = undefined;
    // a misbehaving builder: accepted altered / requestHash altered / encoding altered / bad txid
    for (const tamper of [
      (r: any) => (r.paymentPayload.accepted = { ...r.paymentPayload.accepted, amount: '1' }),
      (r: any) => (r.paymentPayload.payload.requestHash = '00'.repeat(32)),
      (r: any) => (r.paymentPayload.payload.transactionEncoding = 'other'),
      (r: any) => (r.transactionId = 'nope'),
    ]) {
      rig.wasm.tamper = tamper;
      await assert.rejects(rig.client.fetch(`${rig.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'payment_failed');
    }
    assert.equal((await rig.store.list()).length, 0);
    assert.equal(rig.facilitator.settleCalls().length, 0);
  } finally {
    await rig.close();
  }
});

test('no acceptable offer: KAS-only payer facing a token-only paywall', async () => {
  const rig = await startRig({ offers: [{ kind: 'kcc20', asset: TOKEN_B, amount: '700', token: { custody: 'unconditional' } }], capabilities: { kasOnly: true } });
  try {
    await assert.rejects(rig.client.fetch(`${rig.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'no_acceptable_offer');
  } finally {
    await rig.close();
  }
});

test('an unpaid 200 passes through untouched', async () => {
  const rig = await startRig({ paywall: { offers: [NATIVE_OFFER] } });
  try {
    const raw = await fetch(`${rig.base}/report`);
    assert.equal(raw.status, 402);
    const other = await rig.client.paidFetch(`${rig.facilitator.url}/supported`);
    assert.equal(other.response.status, 200);
    assert.equal(other.payment, undefined);
  } finally {
    await rig.close();
  }
});

test('revoke: spends an input of an unsettled artifact back to the payer; refused once settled', async () => {
  const submitted: string[] = [];
  const rig = await startRig({
    settle: () => ({ drop: true }),
    client: { submit: async (tx: string) => (submitted.push(tx), (JSON.parse(tx) as { id: string }).id) },
  });
  try {
    await assert.rejects(rig.client.fetch(`${rig.base}/report`, { paymentId: 'revocable-payment-id-001' }), (e: unknown) => e instanceof KobX402Error && e.code === 'payment_pending');
    const out = await rig.client.revoke('revocable-payment-id-001');
    assert.equal(out.submitted, true);
    assert.equal(submitted.length, 1);
    const rec = (await rig.store.load('revocable-payment-id-001'))!;
    assert.equal(rec.status, 'revoked');
    assert.equal(rec.revokeTransactionId, out.transactionId);
    const spent = JSON.parse(rec.paymentPayload.payload.transaction).inputs[0];
    assert.deepEqual(JSON.parse(submitted[0]!).inputs[0], spent);
    await assert.rejects(rig.client.revoke('revocable-payment-id-001'), (e: unknown) => e instanceof KobX402Error && e.code === 'revoke_failed');
    await assert.rejects(rig.client.revoke('no-such-payment-000000'), (e: unknown) => e instanceof KobX402Error && e.code === 'revoke_failed');
  } finally {
    await rig.close();
  }
  const ok = await startRig();
  try {
    await ok.client.fetch(`${ok.base}/report`, { paymentId: 'settled-payment-id-00001' });
    await assert.rejects(ok.client.revoke('settled-payment-id-00001'), (e: unknown) => e instanceof KobX402Error && e.code === 'revoke_failed');
  } finally {
    await ok.close();
  }
});

test('a duplicate that arrives while the first settlement is in flight gets 409 settlement_pending, and settles nothing twice', async () => {
  const rig = await startRig({ settle: () => ({ delayMs: 250 }) });
  try {
    const url = `${rig.base}/report`;
    const id = 'in-flight-payment-id-0001';
    const first = rig.client.paidFetch(url, { paymentId: id });
    // wait until the first paid request reached the facilitator, then send the same artifact again
    for (let i = 0; i < 100 && rig.facilitator.settleCalls().length === 0; i++) await new Promise((r) => setTimeout(r, 5));
    const dup = await fetch(url, { headers: { 'payment-signature': encodePaymentSignature((await rig.store.load(id))!.paymentPayload) } });
    assert.equal(dup.status, 409);
    assert.equal(((await dup.json()) as any).extensions.kaspa.diagnostic, 'settlement_pending');
    assert.equal((await first).response.status, 200);
    assert.equal(rig.facilitator.settleCalls().length, 1);
  } finally {
    await rig.close();
  }
});

test('wallet flow: kcc20 and swap payments are built unsigned, the wallet signs the requests, only signatures come back', async () => {
  const wallet = { publicKey: '22'.repeat(32), asked: [] as unknown[][], signInputs: async (reqs: any[]) => (wallet.asked.push(reqs), reqs.map((r) => ({ inputIndex: r.inputIndex, signature: '33'.repeat(64) }))) };
  for (const [offers, kind] of [
    [[{ kind: 'kcc20', asset: TOKEN_B, amount: '700', token: { custody: 'unconditional' } }], 'kcc20'],
    [[{ kind: 'swap', receive: 'kas', amount: '50000000', payAssets: [{ asset: TOKEN_A }] }], 'swap'],
  ] as const) {
    const rig = await startRig({
      offers: offers as unknown as OfferSpec[],
      capabilities: { tokens: { [TOKEN_A]: '99', [TOKEN_B]: '1000' } },
      client: { privateKeys: undefined as unknown as string[], wallet },
    });
    try {
      const { response, payment } = await rig.client.paidFetch(`${rig.base}/report`);
      assert.equal(response.status, 200);
      assert.equal(payment?.kind, kind);
      assert.ok(rig.wasm.calls.some((c) => c.method === (kind === 'kcc20' ? 'buildKcc20Unsigned' : 'prepareSwap')));
      assert.ok(rig.wasm.calls.some((c) => c.method === (kind === 'kcc20' ? 'finishKcc20' : 'finishSwap')));
      assert.equal(rig.wasm.calls.some((c) => c.method.startsWith('pay')), false, 'no local-key builder ran');
      assert.equal(rig.wasm.calls.some((c) => (c.arg as { privateKeys?: unknown } | undefined)?.privateKeys !== undefined), false, 'no secret key was handed to the wasm');
      const unsigned = rig.wasm.calls.find((c) => c.method === 'buildKcc20Unsigned' || c.method === 'prepareSwap')!.arg as { payerPublicKey: string };
      assert.equal(unsigned.payerPublicKey, wallet.publicKey);
    } finally {
      await rig.close();
    }
  }
  assert.equal(wallet.asked.length, 2);
});

test('wallet mode cannot pay a native offer (the authorization digest needs a raw Schnorr signature); a revoke needs an artifact', async () => {
  const wallet = { publicKey: '22'.repeat(32), signInputs: async () => [] };
  const rig = await startRig({ client: { privateKeys: undefined as unknown as string[], wallet } });
  try {
    await assert.rejects(rig.client.fetch(`${rig.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'unsupported');
    assert.equal(rig.facilitator.settleCalls().length, 0);
    assert.equal((await rig.store.list()).length, 0);
    await assert.rejects(rig.client.revoke('whatever-payment-id-00001'), (e: unknown) => e instanceof KobX402Error && e.code === 'revoke_failed');
  } finally {
    await rig.close();
  }
});

test('wallet mode revokes an unsettled kcc20 payment: the wallet signs the self-spend (C5 X-10)', async () => {
  const wallet = { publicKey: '22'.repeat(32), asked: [] as unknown[][], signInputs: async (reqs: any[]) => (wallet.asked.push(reqs), reqs.map((r) => ({ inputIndex: r.inputIndex, signature: '33'.repeat(64) }))) };
  const submitted: string[] = [];
  const rig = await startRig({
    offers: [{ kind: 'kcc20', asset: TOKEN_B, amount: '700', token: { custody: 'unconditional' } }] as unknown as OfferSpec[],
    capabilities: { tokens: { [TOKEN_B]: '1000' } },
    settle: () => ({ drop: true }),
    client: { privateKeys: undefined as unknown as string[], wallet, feeRate: 250, submit: async (tx: string) => (submitted.push(tx), (JSON.parse(tx) as { id: string }).id) },
  });
  try {
    await assert.rejects(rig.client.fetch(`${rig.base}/report`, { paymentId: 'wallet-revocable-0000001' }), (e: unknown) => e instanceof KobX402Error && e.code === 'payment_pending');
    const out = await rig.client.revoke('wallet-revocable-0000001');
    assert.equal(out.submitted, true);
    const prep = rig.wasm.calls.find((c) => c.method === 'prepareRevoke')!.arg as { payerPublicKey: string; feeRate?: number; privateKeys?: unknown };
    assert.equal(prep.payerPublicKey, wallet.publicKey);
    assert.equal(prep.feeRate, 250, 'the revoke pays the client rate');
    assert.equal(prep.privateKeys, undefined);
    assert.ok(rig.wasm.calls.some((c) => c.method === 'finishRevoke'));
    assert.equal(rig.wasm.calls.some((c) => c.method === 'revoke'), false, 'no local-key revoke ran');
    assert.equal(wallet.asked.length, 2, 'the payment and the revoke, one wallet round each');
    assert.equal((await rig.store.load('wallet-revocable-0000001'))!.status, 'revoked');
  } finally {
    await rig.close();
  }
});

test('a client with neither keys nor a wallet is refused at construction', () => {
  assert.throws(() => new KobX402Client({ wasm: stubWasm(), network: NETWORK, payerAddress: PAYER, context: staticContext() }), (e: unknown) => e instanceof KobX402Error && e.code === 'bad_request');
});
