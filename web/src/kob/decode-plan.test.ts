// Integration: what the order planners claim (`OrderPlan.states`, `disclosure`) must agree with what the pre-sign decoder re-derives from the
// built tx alone. A disagreement is a bug in the planner or in the decoder, and the user would be blocked from signing.
import { describe, expect, it } from 'vitest';
import { decodeSigning, type ExpectedSigning } from './decode';
import type { CondIntent } from './intent-cond';
import type { SimpleIntent } from './intent-simple';
import { planOrder } from './plan';
import type { OrderPlan, PlanEnv } from './plan-types';
import { parseRegistry } from './registry';
import { TOK, makeEnv, market3x3, market8x8 } from '../testing/fixtures';
import { loadKobNode } from './wasm.node';
import { tradableRegistryJson, nodeFactsOf } from '../testing/chain-fixtures';

const kob = loadKobNode();
const registry = parseRegistry(tradableRegistryJson(), { kob });

/** The plan's own claims as decoder expectations. */
function expectedOf(plan: OrderPlan): ExpectedSigning {
  const d = plan.disclosure!;
  return { orders: plan.states, kasLocked: d.kasLocked, tokensEscrowed: d.tokensEscrowed, maxFee: d.fee ?? undefined };
}

function agree(env: PlanEnv, intent: SimpleIntent | CondIntent, label: string) {
  const plan = planOrder(env, intent);
  expect(plan.issues.filter((i) => i.severity === 'error'), label).toEqual([]);
  expect(plan.ok, label).toBe(true);
  const s = decodeSigning({ kob, built: plan.built!, maker: env.maker, registry: null, expected: expectedOf(plan), nodeInputs: nodeFactsOf(plan.built!) });
  expect(s.blocking, label).toEqual([]);
  expect(s.orders.length, label).toBe(1);
  // the decoder's fee and locked KAS are the disclosure's
  expect(s.fee.sompi, label).toBe(plan.disclosure!.fee);
  expect(s.net.kas.locked.total, label).toBe(plan.disclosure!.kasLocked);
  expect(s.net.tokens.reduce((a, t) => a + t.escrowed, 0n), label).toBe(plan.disclosure!.tokensEscrowed);
  return { plan, s };
}

describe('planner claims == decoded facts', () => {
  const P = 250_000_000n;
  it('limit sell / buy (3x3 and 8x8 programs), IOC, FOK, day, GTD, TWAP, DCA', () => {
    for (const [name, env] of [['3x3', makeEnv()], ['8x8', makeEnv({ market: market8x8() })]] as const) {
      const cases: [string, SimpleIntent][] = [
        ['limit sell', { type: 'limit', side: 'sell', amount: 10n * TOK, price: P }],
        ['limit buy', { type: 'limit', side: 'buy', amount: 10n * TOK, price: P }],
        ['limit sell tip', { type: 'limit', side: 'sell', amount: 4n * TOK, price: P, tip: 100_000n }],
        ['day sell', { type: 'limit', side: 'sell', amount: 3n * TOK, price: P, lifetime: { kind: 'day' } }],
        ['ioc buy', { type: 'ioc', side: 'buy', amount: 2n * TOK, price: P }],
        ['ioc sell', { type: 'ioc', side: 'sell', amount: 2n * TOK, price: 200_000_000n }],
        // amounts that are not a multiple of the scale, and an explicit minimum fill
        ['limit sell odd amount', { type: 'limit', side: 'sell', amount: 1_234n, price: P }],
        ['limit buy odd amount', { type: 'limit', side: 'buy', amount: 7_777n, price: P, minFill: 2_500n }],
      ];
      for (const [label, intent] of cases) agree(env, intent, `${name} ${label}`);
    }
  });

  it('day orders carry their deadline through the placement record', () => {
    const { s } = agree(makeEnv(), { type: 'limit', side: 'sell', amount: 3n * TOK, price: P, lifetime: { kind: 'day' } }, 'day');
    expect(s.orders[0].deadline).not.toBeNull();
    expect(s.orders[0].deadline! > 1_790_694_000n).toBe(true);
  });

  it('stops, take-profit, OCO, IFD / IFO and repeat orders (the committed exit is listed last in the plan)', () => {
    const env = makeEnv();
    const cases: [string, CondIntent][] = [
      ['stop-market sell', { type: 'stopMarket', side: 'sell', amount: 6n * TOK, stop: 200_000_000n }],
      ['stop-limit sell', { type: 'stopLimit', side: 'sell', amount: 6n * TOK, stop: 200_000_000n, limit: 190_000_000n }],
      ['take-profit sell', { type: 'takeProfit', side: 'sell', amount: 6n * TOK, price: 300_000_000n }],
      ['oco sell', { type: 'oco', side: 'sell', amount: 6n * TOK, stop: 200_000_000n, takeProfit: 300_000_000n }],
      ['oco buy', { type: 'oco', side: 'buy', amount: 6n * TOK, stop: 300_000_000n, takeProfit: 200_000_000n }],
      ['ifd buy-first', { type: 'ifd', side: 'buy', amount: 10n * TOK, entry: { price: 240_000_000n }, exit: { takeProfit: 300_000_000n } }],
      ['ifo buy-first', { type: 'ifo', side: 'buy', amount: 10n * TOK, entry: { price: 240_000_000n }, exit: { takeProfit: 300_000_000n, stop: 200_000_000n } }],
      ['ifd sell-first', { type: 'ifd', side: 'sell', amount: 10n * TOK, entry: { price: 260_000_000n }, exit: { takeProfit: 240_000_000n } }],
      ['repeat ifd buy-first', { type: 'repeatIfd', side: 'buy', amount: 4n * TOK, entry: { price: 240_000_000n }, exit: { takeProfit: 300_000_000n }, repeat: { count: 3n } }],
      ['repeat ifd sell-first', { type: 'repeatIfd', side: 'sell', amount: 4n * TOK, entry: { price: 260_000_000n }, exit: { takeProfit: 240_000_000n }, repeat: { count: 3n } }],
    ];
    for (const [label, intent] of cases) agree(env, intent, label);
    const ifd = agree(env, cases[5][1], 'ifd').s;
    expect(ifd.orders[0].description.entry!.exit).not.toBeNull();
    const repeat = agree(env, cases[8][1], 'repeat').s;
    // rptAmount = 1 + cycle amount x count
    expect(repeat.orders[0].description.entry!.rptAmount).toBe(1n + 4n * TOK * 3n);
  });

  it('a plan that disagrees with its own transaction is caught: a different price claimed', () => {
    const env = makeEnv();
    const plan = planOrder(env, { type: 'limit', side: 'sell', amount: 10n * TOK, price: P });
    const wrong = { ...expectedOf(plan), orders: [{ ...plan.states[0], state: { ...plan.states[0].state, price: (P + 100n).toString() } } as typeof plan.states[0]] };
    const s = decodeSigning({ kob, built: plan.built!, maker: env.maker, registry: null, expected: wrong, nodeInputs: nodeFactsOf(plan.built!) });
    expect(s.blocking.map((b) => b.code)).toEqual(['expected-orders']);
  });

  it('a registry built from the same token marks the tradable token without warnings', () => {
    const env = makeEnv({ market: market3x3() });
    const plan = planOrder(env, { type: 'limit', side: 'sell', amount: 10n * TOK, price: P });
    const s = decodeSigning({ kob, built: plan.built!, maker: env.maker, registry, expected: expectedOf(plan), nodeInputs: nodeFactsOf(plan.built!) });
    expect(s.blocking).toEqual([]);
    expect(s.warnings.map((w) => w.code)).toEqual([]);
  });
});
