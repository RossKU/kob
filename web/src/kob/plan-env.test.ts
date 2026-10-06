// Regressions on what a malicious / failing indexer can steer in the planners:
//   the market reference price is cross-checked with the last fill and its source is shown
//   a guard that could not run (indexer failure, hostile omission) is REPORTED and blocks until acknowledged
import { describe, expect, it } from 'vitest';
import { buildPlanEnv } from '../app/env';
import type { Services } from '../app/services';
import { checkSelfTrade } from './guards';
import { planOrder } from './plan';
import type { PlanEnv } from './plan-types';
import { CLOCK, KAS, MAKER_PK, TOK, kob, level, makeEnv, market3x3 } from '../testing/fixtures';
import { parseRegistry } from './registry';
import { tradableRegistryJson } from '../testing/chain-fixtures';

const K = kob();
const registry = parseRegistry(tradableRegistryJson(), { kob: K });
const codes = (p: { issues: { code: string }[] }) => p.issues.map((i) => i.code);

describe('the reference price of market orders', () => {
  const honestBook = { asks: [level(250_000_000n, 50n * TOK)], bids: [level(245_000_000n, 50n * TOK)] };
  const lyingBook = { asks: [level(2_500_000_000n, 50n * TOK)], bids: [level(2_450_000_000n, 50n * TOK)] }; // 25.00 KAS per token instead of 2.50
  const buy = { type: 'market', side: 'buy', amount: 5n * TOK } as const;

  it('a book 10x above the last fill (an independent data path) raises MARKET_REFERENCE_DIVERGES', () => {
    const env: PlanEnv = { ...makeEnv({ book: lyingBook, funding: [100_000n * KAS] }), lastFillPrice: 248_000_000n };
    const p = planOrder(env, buy);
    expect(p.ok).toBe(true);
    expect(codes(p)).toContain('MARKET_REFERENCE_DIVERGES');
    const d = p.issues.find((i) => i.code === 'MARKET_REFERENCE_DIVERGES')!;
    expect(d.severity).toBe('warning');
    expect(d.params).toMatchObject({ lastFill: 248_000_000n });
  });

  it('an honest book next to the last fill raises no divergence warning', () => {
    const env: PlanEnv = { ...makeEnv({ book: honestBook, funding: [100_000n * KAS] }), lastFillPrice: 248_000_000n };
    expect(codes(planOrder(env, buy))).not.toContain('MARKET_REFERENCE_DIVERGES');
    // no last fill known: nothing to compare with (the source line below is still shown)
    expect(codes(planOrder(makeEnv({ book: honestBook, funding: [100_000n * KAS] }), buy))).not.toContain('MARKET_REFERENCE_DIVERGES');
  });

  it('a market order always states where its reference price comes from and the worst price it can pay', () => {
    const p = planOrder(makeEnv({ book: honestBook, funding: [100_000n * KAS] }), buy);
    const s = p.issues.find((i) => i.code === 'MARKET_REFERENCE_SOURCE')!;
    expect(s.severity).toBe('info');
    expect(s.params).toMatchObject({ reference: 250_000_000n });
    expect(BigInt(s.params!.worst as bigint)).toBeGreaterThanOrEqual(250_000_000n);
  });
});

describe('guards that could not run are reported, not silently off', () => {
  const services = (allOrders: () => Promise<unknown>) =>
    ({
      kob: K,
      registry,
      node: { getClock: async () => ({ daa: CLOCK.daa, unixSeconds: CLOCK.unixSeconds, rateMilli: 10_000 }) },
      utxos: { fundingFor: async () => [] },
      tracker: { tokenUtxosFor: async () => [] },
      indexer: {
        book: async () => ({ asks: [], bids: [] }),
        allOrders,
        trades: async () => null,
      },
      config: { features: {} },
    }) as unknown as Services;

  it('buildPlanEnv marks own-orders unavailable when the indexer fails to list the wallet orders', async () => {
    const info = registry.tokens[0];
    const env = await buildPlanEnv(services(async () => { throw new Error('503 (or a hostile indexer that just omits your resting orders)'); }), { pubkey: MAKER_PK }, info);
    expect(env.ownOrders).toEqual([]);
    expect(env.guardsUnavailable).toEqual(['own-orders']);
  });

  it('a healthy indexer leaves no guard marked unavailable', async () => {
    const env = await buildPlanEnv(services(async () => []), { pubkey: MAKER_PK }, registry.tokens[0]);
    expect(env.guardsUnavailable).toBeUndefined();
  });

  it('while guards are unavailable every plan carries a blocking GUARDS_UNAVAILABLE; once acknowledged it is a warning and the plan is ok', () => {
    const base = makeEnv({ funding: [100_000n * KAS] });
    const intent = { type: 'limit', side: 'sell', amount: 1n * TOK, price: 400_000_000n } as const;
    const ok = planOrder(base, intent);
    expect(ok.ok).toBe(true);
    const blocked = planOrder({ ...base, guardsUnavailable: ['own-orders'] }, intent);
    expect(blocked.ok).toBe(false);
    expect(blocked.issues.find((i) => i.code === 'GUARDS_UNAVAILABLE')!.severity).toBe('error');
    const acked = planOrder({ ...base, guardsUnavailable: ['own-orders'], guardsAcknowledged: true }, intent);
    expect(acked.ok).toBe(true);
    expect(acked.issues.find((i) => i.code === 'GUARDS_UNAVAILABLE_ACKNOWLEDGED')!.severity).toBe('warning');
  });

  it('the self-trade guard itself still refuses a crossing order when it has the data', () => {
    const m = market3x3();
    const own = [{ covenantId: 'aa'.repeat(32), side: 'buy' as const, price: 300_000_000n, tip: 0n, amountLeft: 5n * m.scale, active: true }];
    expect(checkSelfTrade(own, { side: 'sell', price: 250_000_000n, tip: 0n }).map((i) => i.code)).toEqual(['SELF_TRADE']);
    // nothing left: nothing to cross
    expect(checkSelfTrade([{ ...own[0]!, amountLeft: 0n }], { side: 'sell', price: 250_000_000n, tip: 0n })).toEqual([]);
  });
});

describe('buildPlanEnv leaves the wallet\'s pair orders out of the KAS book guards', () => {
  const info = registry.tokens[0]!;
  const scale = 10 ** info.decimals;
  // a pair order is listed under both its tokens; it has no KAS price (price / quote null): as a KAS-book order it would be a sell at 0
  const pairAsk = {
    covenant_id: 'c1'.repeat(32), contract: 'KobPair', side: 1, token: info.covenantId, status: 'open', price: null, quote: null, tip: '0',
    scale, min_fill: '1', amount_left: String(2 * scale), initial_amount: String(2 * scale), expired: false,
    pair: { base: info.covenantId, quote: 'bb'.repeat(32), side: 'ask', price: '100', quote_now: '100', amount_left: String(2 * scale), custodies: [] },
  };
  const kasBid = { ...pairAsk, covenant_id: 'c2'.repeat(32), contract: 'KobBid', side: 2, price: '300000000', pair: undefined };
  const services = (mine: unknown[]) =>
    ({
      kob: K,
      registry,
      node: { getClock: async () => ({ daa: CLOCK.daa, unixSeconds: CLOCK.unixSeconds, rateMilli: 10_000 }) },
      utxos: { fundingFor: async () => [] },
      tracker: { tokenUtxosFor: async () => [] },
      indexer: { book: async () => ({ asks: [], bids: [] }), allOrders: async () => mine, trades: async () => null },
      config: { features: {} },
    }) as unknown as Services;

  it('only the KAS-book order is a self-trade reference; the guard stays available', async () => {
    const env = await buildPlanEnv(services([pairAsk, kasBid]), { pubkey: MAKER_PK }, info);
    expect(env.guardsUnavailable).toBeUndefined();
    expect(env.ownOrders).toEqual([{ covenantId: 'c2'.repeat(32), side: 'buy', price: 300_000_000n, tip: 0n, amountLeft: BigInt(2 * scale), active: true }]);
  });
});
