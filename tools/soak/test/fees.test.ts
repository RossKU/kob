// The soak's fee policy glue: config.fees, the bank's feeRate plan (fake generator and the real SDK generator),
// and the unplanned builders (token consolidation, fan-out). The policy itself is tested in web/src/kob/fee-policy.test.ts.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { feeSettingsOf } from '../src/config.ts';
import { planWithFeePolicy, type PayoutOutput, KAS } from '../src/bank-plan.ts';
import { buildAtUrgency } from '../src/fees.ts';
import { feeChoiceOf, normalizePolicy, createFeeService, type FeeEstimate } from '../../../web/src/kob/fee-policy.ts';

// ------------------------------------------------------------------------------------------------ config

test('config.fees: an absent key is the defaults with dynamic ON; keys are independent', () => {
  assert.deepEqual(feeSettingsOf(undefined), { dynamic: true, maxRate: 1000, maxFeeKas: 1 });
  assert.deepEqual(feeSettingsOf({}), { dynamic: true, maxRate: 1000, maxFeeKas: 1 });
  assert.deepEqual(feeSettingsOf({ dynamic: false }), { dynamic: false, maxRate: 1000, maxFeeKas: 1 });
  assert.deepEqual(feeSettingsOf({ maxRate: 400, maxFeeKas: 0.25 }), { dynamic: true, maxRate: 400, maxFeeKas: 0.25 });
  assert.deepEqual(feeSettingsOf({ maxFeeKas: 0 }), { dynamic: true, maxRate: 1000, maxFeeKas: 0 });
});

test('config.fees: an invalid value throws (a typo must not silently change what the soak pays)', () => {
  assert.throws(() => feeSettingsOf({ dynamic: 'yes' as never }), /dynamic/);
  assert.throws(() => feeSettingsOf({ maxRate: 50 }), /maxRate/);
  assert.throws(() => feeSettingsOf({ maxRate: 150.5 }), /maxRate/);
  assert.throws(() => feeSettingsOf({ maxFeeKas: -1 }), /maxFeeKas/);
  assert.throws(() => feeSettingsOf('on' as never), /object/);
});

// ------------------------------------------------------------------------------------------------ x402 payer
// The payer's rate is the x402 SDK's `feeRate` / `feeRateFloor` (packages/kob-x402/test/fee.test.ts covers the pass-through to every
// builder and the one retry at the floor); the soak only sets the HIGH-bucket rate before each payment (bots/x402.ts).

// ------------------------------------------------------------------------------------------------ bank

const addr = 'kaspatest:qz';
const outs: PayoutOutput[] = [{ address: addr, amount: 100n * KAS }];
const fakeCreate = (mass: bigint, log: bigint[] = []) => async (_o: PayoutOutput[], rate: bigint) => {
  log.push(rate);
  return { transactions: [{ feeAmount: rate * mass }, { feeAmount: rate * mass }], summary: {} };
};

test('bank: the plan is generated at the picked rate', async () => {
  const log: bigint[] = [];
  const r = await planWithFeePolicy(fakeCreate(1_000n, log), outs, normalizePolicy(), 130n);
  assert.deepEqual(log, [130n]);
  assert.equal(r.rate, 130n);
  assert.equal(r.fellBackToFloor, false);
  assert.equal(r.cappedFrom, undefined);
});

test('bank: a plan that fails at the picked rate is generated again at the floor', async () => {
  const log: bigint[] = [];
  const create = async (_o: PayoutOutput[], rate: bigint) => {
    log.push(rate);
    if (rate > 100n) throw new Error('Insufficient funds');
    return { transactions: [{ feeAmount: 100n * 1_000n }] };
  };
  const r = await planWithFeePolicy(create, outs, normalizePolicy(), 500n);
  assert.deepEqual(log, [500n, 100n]);
  assert.equal(r.rate, 100n);
  assert.equal(r.fellBackToFloor, true);
  // at the floor an error is the error
  await assert.rejects(
    planWithFeePolicy(async () => { throw new Error('boom'); }, outs, normalizePolicy(), 100n),
    /boom/,
  );
});

test('bank: the total cap regenerates ONCE at floor_div(rate x cap, fee) when one transaction pays more', async () => {
  const log: bigint[] = [];
  // mass 4,000,000 x 400 = 1.6e9 sompi per transaction against the 1 KAS cap -> 400 x 1e8 / 1.6e9 = 25 -> the floor 100
  const r = await planWithFeePolicy(fakeCreate(4_000_000n, log), outs, normalizePolicy(), 400n);
  assert.deepEqual(log, [400n, 100n]);
  assert.equal(r.rate, 100n);
  assert.equal(r.cappedFrom, 400n);
  // mass 500,000 x 400 = 2e8 -> 400 x 1e8 / 2e8 = 200
  const log2: bigint[] = [];
  const r2 = await planWithFeePolicy(fakeCreate(500_000n, log2), outs, normalizePolicy(), 400n);
  assert.deepEqual(log2, [400n, 200n]);
  assert.equal(r2.rate, 200n);
  // no total cap: never regenerated
  const log3: bigint[] = [];
  await planWithFeePolicy(fakeCreate(4_000_000n, log3), outs, normalizePolicy({ maxFeeSompi: 0n }), 400n);
  assert.deepEqual(log3, [400n]);
});

const SDK_DIR = fileURLToPath(new URL('../../../web/vendor/kaspa-node/', import.meta.url));
test('bank: the real SDK generator pays feeRate x mass (feeRate is a number of sompi per gram)', { skip: !existsSync(SDK_DIR + 'kaspa.js') }, async () => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const sdk = createRequire(import.meta.url)(SDK_DIR + 'kaspa.js') as any;
  const network = 'testnet-10';
  const address = new sdk.PrivateKey('01'.repeat(32)).toPublicKey().toAddress(network).toString();
  const entries = Array.from({ length: 10 }, (_, i) => ({
    address,
    outpoint: { transactionId: (i + 1).toString(16).padStart(64, '0'), index: 0 },
    amount: 500_000_000n,
    scriptPublicKey: sdk.payToAddressScript(address),
    blockDaaScore: 1000n,
    isCoinbase: false,
  }));
  const create = (o: PayoutOutput[], rate: bigint) => sdk.createTransactions({ entries, outputs: o, changeAddress: address, priorityFee: 0n, feeRate: Number(rate), networkId: network });
  const payout: PayoutOutput[] = [{ address, amount: 1_000_000_000n }];
  const floor = await planWithFeePolicy(create, payout, normalizePolicy(), 100n);
  const mass = BigInt(floor.result.transactions[0].feeAmount) / 100n;
  assert.ok(mass > 1_000n);
  const dear = await planWithFeePolicy(create, payout, normalizePolicy(), 150n);
  assert.equal(dear.rate, 150n);
  assert.equal(BigInt(dear.result.transactions[0].feeAmount), 150n * mass);
  // a 1,000,000 sompi cap: floor_div(900 x 1e6 / (900 x mass)) = floor_div(1e6 / mass)
  const capped = await planWithFeePolicy(create, payout, normalizePolicy({ maxFeeSompi: 1_000_000n }), 900n);
  assert.equal(capped.rate, 1_000_000n / mass);
  assert.ok(BigInt(capped.result.transactions[0].feeAmount) <= 1_000_000n);
  assert.equal(capped.cappedFrom, 900n);
});

// ------------------------------------------------------------------------------------------------ unplanned builders (consolidate, fan-out)

const estimate: FeeEstimate = { priority: 300, normal: 150, low: 120 };
const envOf = (est: FeeEstimate | null, settings = { dynamic: true, maxRate: 1000, maxFeeKas: 1 }) =>
  ({ fees: createFeeService(settings, { getFeeEstimate: async () => est }) }) as never;

test('buildAtUrgency: LOW bucket rate in the request, the choice remembered for the record', async () => {
  const seen: unknown[] = [];
  const built = await buildAtUrgency(envOf(estimate), 'low', { action: 'sendTokens', tokens: [] }, (r) => {
    seen.push((r as { fee?: unknown }).fee);
    return { fee: { fee: '1000000', feeRate: '120' } } as never;
  });
  assert.deepEqual(seen, [{ feeRate: '120' }]);
  assert.deepEqual(
    { ...feeChoiceOf(built) },
    { urgency: 'low', rate: 120n, fee: 1_000_000n, source: 'estimate', clamped: false, bucketFeerate: 120, overCap: false, maxFeeSompi: 100_000_000n, floor: 100n },
  );
});

test('buildAtUrgency: no estimate or a switched-off policy pays the floor; a build that fails dear is retried at the floor', async () => {
  const rates: unknown[] = [];
  const build = (r: unknown) => {
    const rate = (r as { fee?: { feeRate?: string } }).fee?.feeRate;
    rates.push(rate);
    return { fee: { fee: '5', feeRate: rate ?? '100' } } as never;
  };
  const none = await buildAtUrgency(envOf(null), 'low', {}, build);
  assert.equal(feeChoiceOf(none)!.source, 'floor');
  assert.equal(feeChoiceOf(none)!.reason, 'unavailable');
  const off = await buildAtUrgency(envOf(estimate, { dynamic: false, maxRate: 1000, maxFeeKas: 1 }), 'low', {}, build);
  assert.equal(feeChoiceOf(off)!.reason, 'disabled');
  assert.deepEqual(rates, ['100', '100']);
  rates.length = 0;
  let n = 0;
  const flaky = (r: unknown) => {
    if (n++ === 0) throw new Error('insufficient funds: need 1 sompi, have 0 sompi');
    return build(r);
  };
  const retried = await buildAtUrgency(envOf(estimate), 'normal', {}, flaky);
  assert.deepEqual(rates, ['100']);
  assert.equal(feeChoiceOf(retried)!.reason, 'funds');
  // a failure at the floor is the failure
  await assert.rejects(buildAtUrgency(envOf(null), 'low', {}, () => { throw new Error('nope'); }), /nope/);
});
