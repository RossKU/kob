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
