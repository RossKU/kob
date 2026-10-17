// The payer's funding policy (binding 1.0.0, "Retry and signer policy"): an unattended client pays only under a complete policy
// (funding source, origins, recipients, kinds) plus a ceiling for the merchant asset, or with an explicit `approve`; every list
// the policy pins holds either way; the origin and the resource are the actual request URL's.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { KobX402Client } from '../src/client.ts';
import type { KobX402ClientOptions, PaymentApproval } from '../src/client.ts';
import { KobX402Error } from '../src/errors.ts';
import { MemoryArtifactStore } from '../src/artifact-store.ts';
import { encodePaymentRequired } from '../src/headers.ts';
import { buildOffer } from '../src/offers.ts';
import type { PaymentRequired } from '../src/types.ts';
import { MERCHANT, NATIVE_OFFER, NETWORK, PAYER, SPEND_CAPS, completePolicy, startRaw, startRig, staticContext } from './helpers/env.ts';
import { stubWasm } from './helpers/stub-wasm.ts';

type Rig = Awaited<ReturnType<typeof startRig>>;

/** A second payer on the rig with its own store and `extra` options (no policy unless given). */
function client(rig: Rig, extra: Partial<KobX402ClientOptions>): KobX402Client {
  return new KobX402Client({
    wasm: rig.wasm,
    network: NETWORK,
    payerAddress: PAYER,
    privateKeys: ['01'.repeat(32)],
    context: rig.context,
    capabilities: { maxAmount: SPEND_CAPS },
    store: new MemoryArtifactStore(),
    ...extra,
  });
}

const refused = (e: unknown) => e instanceof KobX402Error && e.code === 'spend_not_authorized';

test('a ceiling alone is not a complete policy: without approve nothing is paid, with it approve is asked (incomplete_policy)', async () => {
  const rig = await startRig();
  try {
    const url = `${rig.base}/report`;
    await assert.rejects(client(rig, {}).paidFetch(url), refused);
    assert.equal(rig.facilitator.settleCalls().length, 0);
    const asked: PaymentApproval[] = [];
    const r = await client(rig, { approve: (q) => (asked.push(q), true) }).paidFetch(url);
    assert.equal(r.response.status, 200);
    assert.deepEqual(asked.map((q) => q.reasons), [['incomplete_policy']]);
    assert.equal(rig.facilitator.settleCalls().length, 1);
  } finally {
    await rig.close();
  }
});

test('every field makes the policy complete: a missing one (or a ceiling missing for the asset) needs approve', async () => {
  const rig = await startRig();
  try {
    const url = `${rig.base}/report`;
    const full = completePolicy(rig.base);
    // complete + ceiling: unattended
    assert.equal((await client(rig, { policy: full }).paidFetch(url)).response.status, 200);
    for (const missing of ['fundingSource', 'allowedOrigins', 'allowedPayTo', 'allowedKinds'] as const) {
      const policy = { ...full };
      delete policy[missing];
      await assert.rejects(client(rig, { policy }).paidFetch(url), refused, missing);
    }
    for (const empty of ['allowedOrigins', 'allowedPayTo', 'allowedKinds'] as const) {
      await assert.rejects(client(rig, { policy: { ...full, [empty]: [] } }).paidFetch(url), (e: unknown) => e instanceof KobX402Error, empty);
    }
    // complete, but no ceiling for KAS
    await assert.rejects(client(rig, { policy: full, capabilities: { maxAmount: {} } }).paidFetch(url), refused);
    assert.equal(rig.facilitator.settleCalls().length, 1);
  } finally {
    await rig.close();
  }
});

test('the pinned lists hold whatever approve says: another origin is refused before sending, another recipient or kind is skipped', async () => {
  const rig = await startRig();
  try {
    const url = `${rig.base}/report`;
    const yes = () => true;
    const other = 'https://merchant.example';
    // origin: refused before the first request (the paywall never sees it)
    const before = rig.handled.length;
    await assert.rejects(
      client(rig, { approve: yes, policy: { allowedOrigins: [other] } }).paidFetch(url),
      (e: unknown) => e instanceof KobX402Error && e.code === 'policy_violation',
    );
    assert.equal(rig.handled.length, before);
    // recipient and kind: the offer is not a candidate, approve cannot bring it back
    await assert.rejects(client(rig, { approve: yes, policy: { allowedPayTo: ['kaspatest:qsomeoneelse'] } }).paidFetch(url), refused);
    await assert.rejects(client(rig, { approve: yes, policy: { allowedKinds: ['kcc20'] } }).paidFetch(url), refused);
    assert.equal(rig.facilitator.settleCalls().length, 0);
  } finally {
    await rig.close();
  }
});

test('the policy is checked against the client: the funding source must match, origins are origins, kinds are known', () => {
  const base = { wasm: stubWasm(), network: NETWORK, payerAddress: PAYER, context: staticContext() } as const;
  assert.throws(() => new KobX402Client({ ...base, privateKeys: ['01'.repeat(32)], policy: { fundingSource: 'wallet' } }), KobX402Error);
  for (const origin of ['https://m.example/path', 'https://m.example/?q=1', 'https://u:p@m.example', 'not a url', 'http://m.example']) {
    assert.throws(() => new KobX402Client({ ...base, privateKeys: ['01'.repeat(32)], policy: { allowedOrigins: [origin] } }), KobX402Error, origin);
  }
  assert.throws(() => new KobX402Client({ ...base, privateKeys: ['01'.repeat(32)], policy: { allowedKinds: ['batch' as never] } }), KobX402Error);
  new KobX402Client({ ...base, privateKeys: ['01'.repeat(32)], policy: { fundingSource: 'local-keys', allowedOrigins: ['https://m.example', 'https://m.example:8443/', 'http://127.0.0.1:8080'] } });
});

test('the 402 must name the requested resource: an advertised resource URL that differs is refused, nothing is signed', async () => {
  const wasm = stubWasm();
  const offer = buildOffer(NATIVE_OFFER, { wasm, network: NETWORK, payTo: MERCHANT, maxTimeoutSeconds: 60, finality: 'accepted' });
  let elsewhere = 'https://elsewhere.example/report';
  const raw = await startRaw((req, res) => {
    const pr: PaymentRequired = { x402Version: 2, resource: { url: elsewhere }, accepts: [offer] };
    res.writeHead(402, { 'content-type': 'application/json', 'payment-required': encodePaymentRequired(pr) }).end(JSON.stringify(pr));
  });
  try {
    const c = new KobX402Client({
      wasm,
      network: NETWORK,
      payerAddress: PAYER,
      privateKeys: ['01'.repeat(32)],
      context: staticContext(),
      capabilities: { maxAmount: SPEND_CAPS },
      policy: completePolicy(raw.base),
    });
    await assert.rejects(c.paidFetch(`${raw.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'invalid_payment_required');
    elsewhere = `${raw.base}/report?other=1`;
    await assert.rejects(c.paidFetch(`${raw.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'invalid_payment_required');
    elsewhere = 'relative/report';
    await assert.rejects(c.paidFetch(`${raw.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'invalid_payment_required');
    assert.equal(wasm.calls.filter((x) => x.method.startsWith('pay')).length, 0, 'nothing was built');
  } finally {
    await raw.close();
  }
});

test('an offer without the upfront payment flow is not paid (binding 1.0.0)', async () => {
  const wasm = stubWasm();
  const offer = buildOffer(NATIVE_OFFER, { wasm, network: NETWORK, payTo: MERCHANT, maxTimeoutSeconds: 60, finality: 'accepted' });
  assert.equal(offer.extra.paymentFlow, 'upfront');
  const { paymentFlow: _drop, ...noFlow } = offer.extra;
  const raw = await startRaw((req, res) => {
    const pr: PaymentRequired = { x402Version: 2, resource: { url: `http://${req.headers.host}${req.url}` }, accepts: [{ ...offer, extra: noFlow as typeof offer.extra }] };
    res.writeHead(402, { 'content-type': 'application/json', 'payment-required': encodePaymentRequired(pr) }).end(JSON.stringify(pr));
  });
  try {
    const c = new KobX402Client({ wasm, network: NETWORK, payerAddress: PAYER, privateKeys: ['01'.repeat(32)], context: staticContext(), capabilities: { maxAmount: SPEND_CAPS }, policy: completePolicy(raw.base) });
    await assert.rejects(c.paidFetch(`${raw.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'no_acceptable_offer');
  } finally {
    await raw.close();
  }
});
