// x402 hardening: the request fingerprint binds what the merchant parses, authorization
// lifetime, https only, revoke trusts the node's txid, artifact overwrite, and the settled transaction is the one sent.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { IncomingMessage, ServerResponse } from 'node:http';
import { httpRequestHash, isStrictProfileJson, normalizeBody } from '../src/canonical.ts';
import { KobX402Client, DEFAULT_AUTH_TTL_SECONDS } from '../src/client.ts';
import { KobX402Error } from '../src/errors.ts';
import { MemoryArtifactStore, FileArtifactStore } from '../src/artifact-store.ts';
import type { ArtifactRecord } from '../src/artifact-store.ts';
import { FacilitatorClient } from '../src/facilitator-client.ts';
import { encodePaymentRequired } from '../src/headers.ts';
import { buildOffer } from '../src/offers.ts';
import { declaredTransactionId } from '../src/server.ts';
import { assertSecureUrl, isLoopbackHost } from '../src/url-policy.ts';
import type { OfferSpec, PaymentRequired } from '../src/types.ts';
import { defaultSettlement } from './helpers/stub-facilitator.ts';
import { MERCHANT, NATIVE_OFFER, NETWORK, PAYER, SPEND_CAPS, kasUtxo, startRaw, startRig, staticContext } from './helpers/env.ts';
import { stubWasm } from './helpers/stub-wasm.ts';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const h = (b: string) => httpRequestHash('POST', 'https://m.example/buy', normalizeBody(b), '00'.repeat(32));

test('bodies a second parser could read differently are fingerprinted as text (byte for byte)', () => {
  // ambiguous JSON: each is hashed as its own text, so it no longer collides with the value the JS parser makes of it
  assert.notEqual(h('{"qty":2,"qty":1}'), h('{"qty":1}'), 'duplicate key');
  assert.notEqual(h('{"qty":2,"qty":1}'), h('{"qty":2}'), 'duplicate key, other order');
  assert.notEqual(h('{"amount":1.0000000000000001}'), h('{"amount":1}'), 'decimal beyond double precision');
  assert.notEqual(h('{"n":1e2}'), h('{"n":100}'), 'exponent');
  assert.notEqual(h('{"n":-0}'), h('{"n":0}'), 'minus zero');
  assert.notEqual(h('{"n":1.0}'), h('{"n":1}'), 'fraction');
  assert.notEqual(h('{"a":{"k":1,"k":1}}'), h('{"a":{"k":1}}'), 'nested duplicate key, same value');
  assert.notEqual(h('{"\\u0061":1,"a":2}'), h('{"a":2}'), 'duplicate key through an escape');
  assert.equal(normalizeBody('{"qty":2,"qty":1}'), '{"qty":2,"qty":1}');
  // the canonical profile keeps its interop: whitespace, key order and string escapes are normal JSON, bound by value
  assert.equal(h('{ "a" : 1,  "b" : [ 2 , 3 ] }'), h('{"b":[2,3],"a":1}'));
  assert.equal(h('{"a":"\\u0041"}'), h('{"a":"A"}'));
  assert.deepEqual(normalizeBody('{"z":1,"a":[2,-3,0]}'), { z: 1, a: [2, -3, 0] });
  // the same key in sibling objects and arrays is fine
  assert.deepEqual(normalizeBody('[{"a":1},{"a":2}]'), [{ a: 1 }, { a: 2 }]);
  assert.deepEqual(normalizeBody('{"a":{"a":1},"b":{"a":2}}'), { a: { a: 1 }, b: { a: 2 } });
  // a real difference is still a difference
  assert.notEqual(h('{"qty":2}'), h('{"qty":1}'));
});

test('isStrictProfileJson on strings that look like numbers or keys', () => {
  assert.equal(isStrictProfileJson('{"s":"1e2","t":"a\\"b","u":"{\\"x\\":1,\\"x\\":2}"}'), true, 'inside a string nothing counts');
  assert.equal(isStrictProfileJson('{"k":[1,2,3],"n":null,"t":true,"f":false}'), true);
  assert.equal(isStrictProfileJson('{"n":9007199254740992}'), false, 'beyond the safe integers');
  assert.equal(isStrictProfileJson('{"n":9007199254740991}'), true);
  assert.equal(isStrictProfileJson('{"n":-9007199254740991}'), true);
  assert.equal(isStrictProfileJson('{"n":0}'), true);
  assert.equal(isStrictProfileJson('{"n":-0}'), false);
  assert.equal(isStrictProfileJson('{"n":2E3}'), false);
});

// ------------------------------------------------------------------------------------------------ https only

test('loopback hosts', () => {
  for (const ok of ['localhost', 'LOCALHOST', 'a.localhost', '127.0.0.1', '127.255.0.9', '::1', '[::1]']) assert.equal(isLoopbackHost(ok), true, ok);
  for (const no of ['example.com', '10.0.0.5', '127.0.0.256', '128.0.0.1', 'localhost.evil.com', '127.0.0.1.evil.com', '::2', '0.0.0.0']) assert.equal(isLoopbackHost(no), false, no);
});

test('assertSecureUrl accepts https, loopback http and an explicit opt-in; nothing else', () => {
  assertSecureUrl('https://merchant.example/x', 'u');
  assertSecureUrl('http://127.0.0.1:8080/x', 'u');
  assertSecureUrl('http://localhost/x', 'u');
  assertSecureUrl('http://[::1]:9/x', 'u');
  assertSecureUrl('http://10.0.0.5/x', 'u', true);
  assert.throws(() => assertSecureUrl('http://merchant.example/x', 'the resource URL'), (e: unknown) => e instanceof KobX402Error && /https/.test(e.message));
  assert.throws(() => assertSecureUrl('ftp://merchant.example/x', 'u', true), KobX402Error);
  assert.throws(() => assertSecureUrl('/relative', 'u'), KobX402Error);
});

test('the payer refuses a plain-http merchant before anything is sent, unless allowed', async () => {
  let calls = 0;
  const f = async (): Promise<Response> => {
    calls++;
    return new Response('{}', { status: 200 });
  };
  const c = new KobX402Client({ network: NETWORK, payerAddress: PAYER, privateKeys: ['01'.repeat(32)], wasm: stubWasm(), context: staticContext(), fetch: f });
  await assert.rejects(c.fetch('http://merchant.example/x'), (e: unknown) => e instanceof KobX402Error && e.code === 'bad_request');
  assert.equal(calls, 0, 'no request left the client');
  const r = await c.fetch('https://merchant.example/x').catch((e) => e);
  void r;
  assert.equal(calls, 1);
  const lax = new KobX402Client({ network: NETWORK, payerAddress: PAYER, privateKeys: ['01'.repeat(32)], wasm: stubWasm(), context: staticContext(), fetch: f, allowInsecureHttp: true });
  await lax.fetch('http://10.0.0.5/x').catch(() => undefined);
  assert.equal(calls, 2);
});

test('the facilitator client refuses a plain-http URL that is not loopback', () => {
  assert.throws(() => new FacilitatorClient({ url: 'http://facilitator.example:8402' }), (e: unknown) => e instanceof KobX402Error && /https/.test(e.message));
  new FacilitatorClient({ url: 'https://facilitator.example' });
  new FacilitatorClient({ url: 'http://127.0.0.1:8402' });
  new FacilitatorClient({ url: 'http://facilitator.internal:8402', allowInsecureHttp: true });
});

// ------------------------------------------------------------------------------------------------ authorization lifetime

const challenge = (spec: OfferSpec, maxTimeoutSeconds: number) => async (req: IncomingMessage, res: ServerResponse) => {
  const url = `http://${req.headers.host}${req.url}`;
  const w = stubWasm();
  const pr: PaymentRequired = { x402Version: 2, resource: { url }, accepts: [buildOffer(spec, { wasm: w, network: NETWORK, payTo: MERCHANT, maxTimeoutSeconds, finality: 'accepted' })] };
  res.writeHead(402, { 'payment-required': encodePaymentRequired(pr) }).end('{}');
};

test('a native / kcc20 authorization signs the shorter of the payer default and the offer timeout', async () => {
  assert.equal(DEFAULT_AUTH_TTL_SECONDS, 600);
  const cases: [number, number | undefined, number][] = [
    [4_294_967_295, undefined, 600], // a merchant asking for ~136 years gets the payer default
    [3_600, undefined, 600],
    [60, undefined, 60], // a shorter offer stays as short
    [3_600, 30, 30],
  ];
  for (const [maxTimeout, ttl, want] of cases) {
    const w = stubWasm();
    const srv = await startRaw(challenge(NATIVE_OFFER, maxTimeout));
    try {
      const c = new KobX402Client({
        network: NETWORK,
        payerAddress: PAYER,
        privateKeys: ['01'.repeat(32)],
        wasm: w,
        context: { load: async () => ({ utxos: [kasUtxo(0)] }) },
        capabilities: { maxAmount: SPEND_CAPS },
        ...(ttl !== undefined ? { authorizationTtlSeconds: ttl } : {}),
      });
      await c.fetch(srv.base + '/x').catch(() => undefined);
      const arg = w.calls.find((x) => x.method === 'payNative')!.arg as { ttlSeconds?: number };
      assert.equal(arg.ttlSeconds, want, `maxTimeoutSeconds ${maxTimeout}, option ${String(ttl)}`);
    } finally {
      await srv.close();
    }
  }
});

test('a bad authorizationTtlSeconds is refused before signing', async () => {
  const w = stubWasm();
  const srv = await startRaw(challenge(NATIVE_OFFER, 60));
  try {
    for (const bad of [0, -5, 1.5, Number.NaN]) {
      const c = new KobX402Client({
        network: NETWORK,
        payerAddress: PAYER,
        privateKeys: ['01'.repeat(32)],
        wasm: w,
        context: { load: async () => ({ utxos: [kasUtxo(0)] }) },
        capabilities: { maxAmount: SPEND_CAPS },
        authorizationTtlSeconds: bad,
      });
      await assert.rejects(c.fetch(srv.base + '/x'), (e: unknown) => e instanceof KobX402Error && e.code === 'bad_request', String(bad));
    }
    assert.equal(w.calls.filter((x) => x.method === 'payNative').length, 0);
  } finally {
    await srv.close();
  }
});

// ------------------------------------------------------------------------------------------------ revoke

async function signedRecord(): Promise<{ rig: Awaited<ReturnType<typeof startRig>>; rec: ArtifactRecord }> {
  const rig = await startRig();
  await rig.client.paidFetch(`${rig.base}/r`);
  const settled = (await rig.store.list())[0]!;
  const rec: ArtifactRecord = { ...settled, paymentId: 'revoke-target-0123456789ab', status: 'signed' };
  return { rig, rec };
}

test('revoke() never takes the node\'s word for the revoke transaction id', async () => {
  const { rig, rec } = await signedRecord();
  try {
    const store = new MemoryArtifactStore();
    await store.save(rec);
    let lie = true;
    const c = new KobX402Client({
      network: NETWORK,
      payerAddress: PAYER,
      privateKeys: ['01'.repeat(32)],
      wasm: rig.wasm,
      context: rig.context,
      store,
      submit: async (tx) => (lie ? 'ee'.repeat(32) : (JSON.parse(tx) as { id: string }).id),
    });
    await assert.rejects(c.revoke(rec.paymentId), (e: unknown) => e instanceof KobX402Error && e.code === 'revoke_failed' && /another transaction id/.test(e.message));
    assert.equal((await store.load(rec.paymentId))!.status, 'signed', 'the artifact stays live');
    lie = false;
    const ok = await c.revoke(rec.paymentId);
    assert.equal(ok.submitted, true);
    const after = (await store.load(rec.paymentId))!;
    assert.equal(after.status, 'revoked');
    assert.equal(after.revokeTransactionId, ok.transactionId);
  } finally {
    await rig.close();
  }
});

// ------------------------------------------------------------------------------------------------ artifact overwrite

test('a payment id never overwrites the artifact of another transaction (memory and file stores)', async () => {
  const { rig, rec } = await signedRecord();
  const dir = mkdtempSync(join(tmpdir(), 'kob-x402-store-'));
  try {
    for (const store of [new MemoryArtifactStore(), new FileArtifactStore(dir)]) {
      await store.save(rec);
      // an update of the same transaction is fine
      await store.save({ ...rec, status: 'pending', note: 'n' });
      await store.update(rec.paymentId, { status: 'rejected' });
      // another transaction under the same id is refused, the first artifact survives
      const other: ArtifactRecord = { ...rec, transactionId: 'cc'.repeat(32) };
      await assert.rejects(store.save(other), (e: unknown) => e instanceof KobX402Error && e.code === 'artifact_store' && /not overwritten/.test(e.message));
      const kept = (await store.load(rec.paymentId))!;
      assert.equal(kept.transactionId, rec.transactionId);
      assert.equal(kept.status, 'rejected');
      // a different id is a different artifact
      await store.save({ ...other, paymentId: 'another-id-0123456789ab' });
      assert.equal((await store.list()).length, 2);
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
    await rig.close();
  }
});

// ------------------------------------------------------------------------------------------------ settled transaction

test('declaredTransactionId reads the id a safe-JSON transaction declares', () => {
  assert.equal(declaredTransactionId(JSON.stringify({ id: 'AB'.repeat(32) })), 'ab'.repeat(32));
  assert.equal(declaredTransactionId(JSON.stringify({ id: 'xyz' })), undefined);
  assert.equal(declaredTransactionId(JSON.stringify({ version: 1 })), undefined);
  assert.equal(declaredTransactionId('not json'), undefined);
  assert.equal(declaredTransactionId('null'), undefined);
});

test('the paywall does not serve a payment the facilitator settled as ANOTHER transaction', async () => {
  const rig = await startRig({
    settle: (_n, body) => ({ body: { ...defaultSettlement(body), transaction: 'dd'.repeat(32) } }),
  });
  try {
    const err = await rig.client.paidFetch(`${rig.base}/report`).catch((e) => e);
    assert.ok(err instanceof KobX402Error && err.code === 'payment_pending', 'the payer got a 502 without a settlement: the artifact stays stored');
    assert.equal(err.status, 502);
    assert.equal(rig.handled.length, 0, 'the resource handler never ran');
  } finally {
    await rig.close();
  }
});

test('a facilitator that settles the transaction that was sent is served as before', async () => {
  const rig = await startRig();
  try {
    const out = await rig.client.paidFetch(`${rig.base}/report`);
    assert.equal(out.response.status, 200);
    assert.equal(rig.handled.length, 1);
  } finally {
    await rig.close();
  }
});
