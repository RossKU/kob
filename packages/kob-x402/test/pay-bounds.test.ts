// What a payer bounds and approves: swap bounds are counted per pay asset, never in the units of whichever asset the
// merchant happens to list first.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { KobX402Error } from '../src/errors.ts';
import type { OfferSpec } from '../src/types.ts';
import { TOKEN_A, TOKEN_B, startRig } from './helpers/env.ts';

const TOKEN_FIRST: OfferSpec[] = [
  { kind: 'swap', receive: 'kcc20', asset: TOKEN_B, amount: '700', token: { custody: 'unconditional' }, payAssets: [{ asset: TOKEN_A }, { asset: 'KAS' }] },
];

test('a KAS bound pays a swap with KAS even when the merchant lists a held token first', async () => {
  const rig = await startRig({ offers: TOKEN_FIRST, capabilities: { tokens: { [TOKEN_A]: '1000000000' } }, client: { maxPay: { KAS: '500000000' } } });
  try {
    await rig.client.paidFetch(`${rig.base}/report`);
    const call = rig.wasm.calls.find((c) => c.method === 'paySwap')!.arg as { payAsset: string; maxPayAmount: string };
    assert.equal(call.payAsset, 'KAS', 'the bounded asset is spent');
    assert.equal(call.maxPayAmount, '500000000');
    const pre = rig.wasm.calls.find((c) => c.method === 'preflight')!.arg as { maxPay: string; maxPayAsset: string };
    assert.deepEqual([pre.maxPay, pre.maxPayAsset], ['500000000', 'KAS']);
  } finally {
    await rig.close();
  }
});

test('the KAS-only shorthand maxPayAmount never bounds a token', async () => {
  const rig = await startRig({
    offers: [{ ...TOKEN_FIRST[0]!, payAssets: [{ asset: TOKEN_A }] } as OfferSpec],
    capabilities: { tokens: { [TOKEN_A]: '1000000000' } },
    client: { maxPay: undefined, maxPayAmount: '500000000' },
  });
  try {
    await assert.rejects(rig.client.paidFetch(`${rig.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'spend_not_authorized');
    assert.equal(rig.wasm.calls.filter((c) => c.method === 'paySwap').length, 0, 'nothing built');
  } finally {
    await rig.close();
  }
});

test('a token bound is passed in that token\'s units with the asset it counts', async () => {
  const rig = await startRig({ offers: TOKEN_FIRST, capabilities: { tokens: { [TOKEN_A]: '1000000000' } }, client: { maxPay: { [TOKEN_A]: '900' } } });
  try {
    await rig.client.paidFetch(`${rig.base}/report`);
    const call = rig.wasm.calls.find((c) => c.method === 'paySwap')!.arg as { payAsset: string; maxPayAmount: string };
    assert.deepEqual([call.payAsset, call.maxPayAmount], [TOKEN_A, '900']);
    const pre = rig.wasm.calls.find((c) => c.method === 'preflight')!.arg as { maxPay: string; maxPayAsset: string };
    assert.deepEqual([pre.maxPay, pre.maxPayAsset], ['900', TOKEN_A]);
  } finally {
    await rig.close();
  }
});

test('approve is asked after the quote with what the swap costs; without a bound a swap is not built, whatever approve says', async () => {
  const offers: OfferSpec[] = [{ kind: 'swap', receive: 'kas', amount: '50000000', payAssets: [{ asset: TOKEN_A }] }];
  const seen: unknown[] = [];
  const approve = (a: { reasons: string[]; cost: unknown }) => (seen.push({ reasons: a.reasons, cost: a.cost }), true);
  // no bound: refused before anything is built, approve is never asked
  const unbounded = await startRig({ offers, capabilities: { maxAmount: {}, tokens: { [TOKEN_A]: '99' } }, client: { maxPay: undefined, approve } });
  try {
    await assert.rejects(unbounded.client.paidFetch(`${unbounded.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'spend_not_authorized');
    assert.equal(unbounded.wasm.calls.filter((c) => c.method === 'paySwap').length, 0);
    assert.deepEqual(seen, []);
  } finally {
    await unbounded.close();
  }
  // bounded but no ceiling for the merchant asset: approve decides on the built payment's cost
  const bounded = await startRig({ offers, capabilities: { maxAmount: {}, tokens: { [TOKEN_A]: '99' } }, client: { maxPay: { [TOKEN_A]: '5000' }, approve } });
  try {
    const r = await bounded.client.paidFetch(`${bounded.base}/report`);
    assert.equal(r.response.status, 200);
    const order = bounded.wasm.calls.map((c) => c.method);
    assert.ok(order.indexOf('paySwap') >= 0 && order.indexOf('preflight') > order.indexOf('paySwap'), 'built and preflighted');
    assert.equal(seen.length, 1);
    const { reasons, cost } = seen[0] as unknown as { reasons: string[]; cost: Record<string, string> };
    assert.deepEqual(reasons, ['no_spend_cap']);
    assert.equal(cost.asset, 'KAS');
    assert.equal(cost.amount, '50000000');
    assert.equal(cost.payAsset, TOKEN_A);
    assert.equal(typeof cost.feeSompi, 'string');
  } finally {
    await bounded.close();
  }
});

test('the KAS ceiling covers the network fee: an offer at the ceiling whose fee goes past it is not paid', async () => {
  const at = await startRig({ capabilities: { maxAmount: { KAS: '50001999' } }, client: { maxPay: undefined } });
  try {
    await assert.rejects(at.client.paidFetch(`${at.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'spend_not_authorized');
    assert.deepEqual(await at.store.list(), [], 'built, refused, never stored');
    assert.equal(at.facilitator.settleCalls().length, 0, 'never sent');
  } finally {
    await at.close();
  }
  const covered = await startRig({ capabilities: { maxAmount: { KAS: '50002000' } }, client: { maxPay: undefined } });
  try {
    assert.equal((await covered.client.paidFetch(`${covered.base}/report`)).response.status, 200);
  } finally {
    await covered.close();
  }
});

test('a token payment shows its carrier and fee to approve and holds them to the KAS ceiling', async () => {
  const offers: OfferSpec[] = [{ kind: 'kcc20', asset: TOKEN_A, amount: '1', token: { custody: 'unconditional', carrier: '150000000' } }];
  const seen: { reasons: string[]; cost: Record<string, string> }[] = [];
  const caps = { maxAmount: { [TOKEN_A]: '10' }, tokens: { [TOKEN_A]: '100' } };
  // no KAS ceiling: the token ceiling alone does not cover the KAS the payment takes; approve sees it
  const ask = await startRig({ offers, capabilities: caps, client: { maxPay: undefined, approve: (a) => (seen.push(a as never), true) } });
  try {
    assert.equal((await ask.client.paidFetch(`${ask.base}/report`)).response.status, 200);
    assert.equal(seen.length, 1);
    assert.deepEqual(seen[0]!.reasons, ['no_kas_cap']);
    assert.equal(seen[0]!.cost.carrierSompi, '150000000');
    assert.equal(seen[0]!.cost.feeSompi, '2000');
    assert.equal(seen[0]!.cost.kasSpent, '150002000');
  } finally {
    await ask.close();
  }
  // a KAS ceiling below carrier + fee refuses it
  const low = await startRig({ offers, capabilities: { ...caps, maxAmount: { ...caps.maxAmount, KAS: '150001999' } }, client: { maxPay: undefined } });
  try {
    await assert.rejects(low.client.paidFetch(`${low.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'spend_not_authorized');
    assert.equal(low.facilitator.settleCalls().length, 0);
  } finally {
    await low.close();
  }
});
