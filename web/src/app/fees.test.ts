// The fee policy at the app level: config.fees -> policy + oracle (feeServiceFor), and what buildPlanEnv / buildCancelEnv hand to the planners.
import { describe, expect, it } from 'vitest';
import { mergeConfig } from '../config';
import { buildCancelEnv, buildPlanEnv, readFees } from './env';
import { feeServiceFor, type Services } from './services';
import { parseRegistry } from '../kob/registry';
import { MockNode } from '../testing/mock-node';
import { CLOCK, MAKER_PK, kob } from '../testing/fixtures';
import { tradableRegistryJson } from '../testing/chain-fixtures';

const K = kob();
const registry = parseRegistry(tradableRegistryJson(), { kob: K });
const estimate = { priority: 300, normal: 150, low: 110 };

function services(fees: Services['fees'] | undefined, node: Partial<MockNode> = {}): Services {
  return {
    kob: K,
    registry,
    node: { getClock: async () => ({ daa: CLOCK.daa, unixSeconds: CLOCK.unixSeconds, rateMilli: 10_000 }), ...node },
    utxos: { fundingFor: async () => [] },
    tracker: { tokenUtxosFor: async () => [] },
    indexer: null,
    config: { features: {} },
    ...(fees ? { fees } : {}),
  } as unknown as Services;
}

describe('feeServiceFor', () => {
  it('the default config is dynamic: floor 100, maxRate 1000, 1 KAS per transaction, an oracle on the node', async () => {
    const node = new MockNode();
    node.feeEstimate = estimate;
    const f = feeServiceFor(mergeConfig(), node);
    expect(f.policy).toMatchObject({ dynamic: true, floor: 100n, maxRate: 1000n, maxFeeSompi: 100_000_000n });
    expect(f.oracle).not.toBeNull();
    expect(await f.oracle!.get()).toEqual(estimate);
  });

  it('config.fees maps to the policy (KAS -> sompi, 0 = no total cap)', () => {
    const f = feeServiceFor(mergeConfig({ injected: { fees: { maxRate: 400, maxFeeKas: 0.25 } } }), new MockNode());
    expect(f.policy).toMatchObject({ maxRate: 400n, maxFeeSompi: 25_000_000n });
    expect(feeServiceFor(mergeConfig({ injected: { fees: { maxFeeKas: 0 } } }), new MockNode()).policy.maxFeeSompi).toBe(0n);
  });

  it('dynamic: false has no oracle, so the node is never asked', async () => {
    const node = new MockNode();
    node.feeEstimate = estimate;
    const f = feeServiceFor(mergeConfig({ injected: { fees: { dynamic: false } } }), node);
    expect(f.oracle).toBeNull();
    expect((await readFees({ fees: f })).estimate).toBeNull();
    expect(node.feeEstimateCalls).toBe(0);
  });

  it('a node that cannot answer (no method, null, throwing) gives no estimate and never an error', async () => {
    expect((await readFees({ fees: feeServiceFor(mergeConfig(), {}) })).estimate).toBeNull();
    const none = new MockNode();
    expect((await readFees({ fees: feeServiceFor(mergeConfig(), none) })).estimate).toBeNull();
    const boom = { getFeeEstimate: async () => { throw new Error('method not found'); } };
    expect((await readFees({ fees: feeServiceFor(mergeConfig(), boom) })).estimate).toBeNull();
  });
});

describe('readFees / buildPlanEnv / buildCancelEnv', () => {
  it('reads the estimate through the cache: one node call serves several environments', async () => {
    const node = new MockNode();
    node.feeEstimate = estimate;
    const s = services(feeServiceFor(mergeConfig(), node), node);
    const info = registry.tokens[0]!;
    const a = await buildPlanEnv(s, { pubkey: MAKER_PK }, info);
    const b = await buildCancelEnv(s, { pubkey: MAKER_PK });
    expect(a.fees!.estimate).toEqual(estimate);
    expect(b.fees!.estimate).toEqual(estimate);
    expect(a.fees!.policy.dynamic).toBe(true);
    expect(node.feeEstimateCalls).toBe(1);
  });

  it('services without a fee service (older fakes): the floor policy, planners pay what they always paid', async () => {
    const info = registry.tokens[0]!;
    const env = await buildPlanEnv(services(undefined), { pubkey: MAKER_PK }, info);
    expect(env.fees).toEqual({ policy: expect.objectContaining({ dynamic: false }), estimate: null });
  });

});
