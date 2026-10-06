// Fee rates of the payer's transactions (src/fee.ts): the node estimate's bucket by urgency, clamped like the executor's
// policy (crates/kob-executor/src/fee.rs), and the client passing it to every builder, retried once at the floor.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { MIN_FEE_RATE, feeRateFromEstimate, parseFeeEstimate, resolveFeeRate, withFeeFloor, withFeeFloorAsync } from '../src/fee.ts';
import { payIntent } from '../src/intent.ts';
import type { IntentPayRequest, KobWasm, PayRequest } from '../src/wasm.ts';
import { BINDING_INTENT } from '../src/types.ts';
import { startRig } from './helpers/env.ts';

const RAW = {
  estimate: {
    priorityBucket: { feerate: 412.3, estimatedSeconds: 0.2 },
    normalBuckets: [{ feerate: 150.01, estimatedSeconds: 9 }, { feerate: 120, estimatedSeconds: 30 }],
    lowBuckets: [{ feerate: 100, estimatedSeconds: 600 }],
  },
};

test('parseFeeEstimate: the priority bucket, the first normal and the first low; empty lists take the next faster bucket', () => {
  assert.deepEqual(parseFeeEstimate(RAW), { priority: 412.3, normal: 150.01, low: 100 });
  assert.deepEqual(parseFeeEstimate(RAW.estimate), { priority: 412.3, normal: 150.01, low: 100 });
  assert.deepEqual(parseFeeEstimate({ priorityBucket: { feerate: 300 }, normalBuckets: [], lowBuckets: [] }), { priority: 300, normal: 300, low: 300 });
  assert.throws(() => parseFeeEstimate({}), /priorityBucket/);
  assert.throws(() => parseFeeEstimate({ priorityBucket: { feerate: 0 } }), /positive/);
  assert.throws(() => parseFeeEstimate({ priorityBucket: { feerate: 200 }, normalBuckets: [{ feerate: Number.NaN }] }), /positive/);
});

test('feeRateFromEstimate: ceil(bucket) of the urgency, clamped to [floor, max(floor, maxRate)]; no estimate = the floor', () => {
  assert.equal(feeRateFromEstimate(RAW, 'high'), 413);
  assert.equal(feeRateFromEstimate(RAW, 'normal'), 151);
  assert.equal(feeRateFromEstimate(RAW, 'low'), MIN_FEE_RATE);
  assert.equal(feeRateFromEstimate(parseFeeEstimate(RAW), 'high', { maxRate: 300 }), 300);
  assert.equal(feeRateFromEstimate(RAW, 'high', { floor: 500 }), 500, 'the floor wins');
  assert.equal(feeRateFromEstimate(RAW, 'high', { floor: 200, maxRate: 150 }), 200, 'the floor wins over a lower maxRate');
  assert.equal(feeRateFromEstimate(undefined, 'high'), MIN_FEE_RATE);
  assert.equal(feeRateFromEstimate(undefined, 'high', { floor: 250 }), 250);
  assert.throws(() => feeRateFromEstimate(RAW, 'high', { floor: 99 }), /relay floor/);
});

test('resolveFeeRate: a number, a function, undefined; a rate below the relay floor or not an integer is refused', async () => {
  assert.equal(await resolveFeeRate(300), 300);
  assert.equal(await resolveFeeRate(async () => 250), 250);
  assert.equal(await resolveFeeRate(() => undefined), undefined);
  assert.equal(await resolveFeeRate(undefined), undefined);
  await assert.rejects(resolveFeeRate(99), /at least 100/);
  await assert.rejects(resolveFeeRate(150.5), /integer/);
});

test('withFeeFloor: a failure above the floor is retried ONCE at the floor; at the floor (or without a rate) the error stands', async () => {
  const seen: (number | undefined)[] = [];
  const build = (r: number | undefined): string => {
    seen.push(r);
    if (r === undefined || r > 100) throw new Error('fee above the bound');
    return `ok@${r}`;
  };
  assert.equal(withFeeFloor(400, undefined, build), 'ok@100');
  assert.deepEqual(seen, [400, 100]);
  seen.length = 0;
  assert.throws(() => withFeeFloor(100, 100, () => { seen.push(100); throw new Error('short'); }), /short/);
  assert.deepEqual(seen, [100]);
  assert.throws(() => withFeeFloor(undefined, 100, build), /bound/);
  assert.equal(await withFeeFloorAsync(300, 200, async (r) => { if (r! > 200) throw new Error('x'); return r; }), 200);
});

test('client: every payment is built at the feeRate the source names; one that fails at it is rebuilt at feeRateFloor', async () => {
  let rate = 333;
  const rig = await startRig({ client: { feeRate: () => rate, feeRateFloor: 120 } });
  try {
    const seen: (number | undefined)[] = [];
    const orig = rig.wasm.payNative.bind(rig.wasm);
    let failAbove = Infinity;
    rig.wasm.payNative = (req: PayRequest) => {
      seen.push(req.feeRate);
      if ((req.feeRate ?? 0) > failAbove) throw new Error('the fee exceeds maxFeeSompi');
      return orig(req);
    };
    const a = await rig.client.paidFetch(`${rig.base}/report?n=1`);
    assert.equal(a.response.status, 200);
    assert.deepEqual(seen, [333]);
    rate = 900;
    failAbove = 500;
    const b = await rig.client.paidFetch(`${rig.base}/report?n=2`);
    assert.equal(b.response.status, 200);
    assert.deepEqual(seen, [333, 900, 120], 'rebuilt once at the floor');
  } finally {
    await rig.close();
  }
});

test('payIntent: options.feeRate reaches the builder; a creation that fails at it is rebuilt once at feeRateFloor', () => {
  const seen: (number | undefined)[] = [];
  const wasm = {
    payIntent(req: IntentPayRequest) {
      seen.push(req.options.feeRate);
      if ((req.options.feeRate ?? 0) > 200) throw new Error('funds short of the dearer fee');
      return { built: req.options.feeRate } as never;
    },
  } as unknown as KobWasm;
  const req = {
    requirements: { extra: { route: { binding: BINDING_INTENT } } },
    requestHash: '00'.repeat(32),
    payAsset: 'KAS',
    utxos: [],
    nowMs: 1,
    options: { maxPay: '1', feeRate: 450 },
    feeRateFloor: 150,
  } as unknown as IntentPayRequest;
  assert.deepEqual(payIntent(wasm, req), { built: 150 });
  assert.deepEqual(seen, [450, 150]);
  assert.equal(req.options.feeRate, 450, 'the caller request is not mutated');
});
