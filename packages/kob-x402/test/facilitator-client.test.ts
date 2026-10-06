import { test } from 'node:test';
import assert from 'node:assert/strict';
import { FacilitatorClient } from '../src/facilitator-client.ts';
import { KobX402Error } from '../src/errors.ts';
import type { FacilitatorRequest } from '../src/types.ts';
import { startStubFacilitator, failure } from './helpers/stub-facilitator.ts';

const req = (): FacilitatorRequest =>
  ({
    x402Version: 2,
    paymentPayload: { payload: { transaction: '{"id":"' + 'ab'.repeat(32) + '"}', profile: 'standard-native' } },
    paymentRequirements: { network: 'kaspa:testnet-10', amount: '5' },
    requestHash: 'cd'.repeat(32),
  }) as unknown as FacilitatorRequest;

test('supported / verify / settle with the API key', async () => {
  const f = await startStubFacilitator({ apiKey: 'k1' });
  try {
    const c = new FacilitatorClient({ url: f.url + '/', apiKey: 'k1' });
    assert.equal((await c.supported()).kinds[0]?.scheme, 'exact');
    assert.equal((await c.verify(req())).isValid, true);
    const s = await c.settle(req());
    assert.equal(s.success, true);
    assert.equal(f.calls.every((x) => x.headers.authorization === 'Bearer k1'), true);
    assert.equal((f.calls[2]!.body as FacilitatorRequest).requestHash, 'cd'.repeat(32));

    const custom = new FacilitatorClient({ url: f.url, apiKey: 'k1', apiKeyHeader: 'x-api-key' });
    await assert.rejects(custom.supported(), (e: unknown) => e instanceof KobX402Error && e.status === 401);
    const bad = new FacilitatorClient({ url: f.url, apiKey: 'wrong' });
    await assert.rejects(bad.supported(), (e: unknown) => e instanceof KobX402Error && e.status === 401);
  } finally {
    await f.close();
  }
});

test('a failure settlement is returned whatever the HTTP status; a non-settlement body is an error', async () => {
  const f = await startStubFacilitator({ settle: (n) => (n === 1 ? { status: 409, body: failure('order_conflict', true, 'x') } : { status: 200, body: { hello: 'world' } }) });
  try {
    const c = new FacilitatorClient({ url: f.url });
    const s = await c.settle(req());
    assert.equal(s.success, false);
    assert.equal((s.extensions?.kaspa as { diagnostic: string }).diagnostic, 'order_conflict');
    await assert.rejects(c.settle(req()), (e: unknown) => e instanceof KobX402Error && e.code === 'facilitator');
  } finally {
    await f.close();
  }
});

test('network errors are retried for idempotent calls only', async () => {
  const f = await startStubFacilitator();
  const url = f.url;
  await f.close(); // nothing listens any more
  let attempts = 0;
  const counting = async (u: string, init?: RequestInit): Promise<Response> => {
    attempts++;
    return fetch(u, init);
  };
  const c = new FacilitatorClient({ url, retries: 2, retryDelayMs: 1, fetch: counting });
  await assert.rejects(c.supported(), (e: unknown) => e instanceof KobX402Error && /unreachable/.test(e.message));
  assert.equal(attempts, 3, '/supported: 1 try + 2 retries');
  attempts = 0;
  await assert.rejects(c.verify(req()));
  assert.equal(attempts, 3, '/verify is read-only: retried');
  attempts = 0;
  await assert.rejects(c.settle(req()));
  assert.equal(attempts, 1, '/settle is not retried by default');
  attempts = 0;
  const retrying = new FacilitatorClient({ url, retries: 1, retryDelayMs: 1, retrySettle: true, fetch: counting });
  await assert.rejects(retrying.settle(req()));
  assert.equal(attempts, 2, 'retrySettle opts in');
});

test('an HTTP error status is not retried; a timeout counts as a network error', async () => {
  const f = await startStubFacilitator({ settle: () => ({ delayMs: 300 }) });
  try {
    let attempts = 0;
    const counting = async (u: string, init?: RequestInit): Promise<Response> => (attempts++, fetch(u, init));
    // 404 is a final answer
    const c = new FacilitatorClient({ url: f.url + '/nope', retries: 2, retryDelayMs: 1, fetch: counting });
    await assert.rejects(c.supported(), (e: unknown) => e instanceof KobX402Error && e.status === 404);
    assert.equal(attempts, 1);
    // settle timeout
    const t = new FacilitatorClient({ url: f.url, settleTimeoutMs: 50 });
    await assert.rejects(t.settle(req()), (e: unknown) => e instanceof KobX402Error && /unreachable/.test(e.message));
  } finally {
    await f.close();
  }
});
