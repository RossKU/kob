import { describe, expect, it } from 'vitest';
import { planOrder } from '../../kob/plan';
import { errors, type OrderPlan, type PlanEnv } from '../../kob/plan-types';
import type { Intent } from '../../kob/plan';
import { makePairEnv } from '../../kob/orders/pair-fixtures';
import { quoteOf } from '../../kob/units';
import { CTX, TOKEN, ticketEnv } from './ticket-fixtures';
import { MAX_KAS_RESERVE, MAX_QUOTE_SLACK, fundsShort, maxBuyAmount } from './max-amount';
import { expandLimitLadder } from './ladder';

const KAS = 100_000_000n;
const PRICE = 230_000_000n; // 2.3 KAS per token
const sum = (xs: bigint[]) => xs.reduce((a, b) => a + b, 0n);
const balanceOf = (env: PlanEnv) => sum(env.funding.map((u) => BigInt(u.amount)));
const limit = (amount: bigint, price = PRICE): Intent => ({ type: 'limit', side: 'buy', amount, price });

/** what the wallet spends of its KAS for the plan: the escrow and carriers plus the network fee */
const spent = (p: OrderPlan) => p.disclosure!.kasLocked + (p.disclosure!.fee ?? 0n);

function maxFor(env: PlanEnv, price: bigint, ladder?: bigint[]): bigint {
  const balance = balanceOf(env);
  return maxBuyAmount({
    upper: (balance * CTX.scale) / price + 1n,
    conservative: 0n,
    kasBalance: balance,
    ladderPrices: ladder ?? null,
    planAt: (n) => planOrder(env, limit(n, price)),
  });
}

describe('max buy on a KAS market', () => {
  it('is the largest amount whose exact cost leaves the reserve, and one more base unit does not', () => {
    for (const funds of [[40n * KAS], [123n * KAS + 4_567n], [5n * KAS, 7n * KAS, 9n * KAS]]) {
      const env = ticketEnv({ funding: funds });
      const max = maxFor(env, PRICE);
      expect(max).toBeGreaterThan(0n);
      const at = planOrder(env, limit(max));
      expect(errors(at), 'plan at max').toEqual([]);
      expect(spent(at) + MAX_KAS_RESERVE).toBeLessThanOrEqual(balanceOf(env));
      // the next base unit breaks the budget: either the planner refuses it or the reserve would be eaten
      const over = planOrder(env, limit(max + 1n));
      const eaten = over.built && over.disclosure ? spent(over) + MAX_KAS_RESERVE > balanceOf(env) : fundsShort(over);
      expect(eaten, 'plan at max + 1').toBe(true);
    }
  });

  it('is below what the balance buys with nothing reserved, by about the carriers and the fee', () => {
    const env = ticketEnv({ funding: [40n * KAS] });
    const max = maxFor(env, PRICE);
    const loose = (40n * KAS * CTX.scale) / PRICE;
    expect(max).toBeLessThan(loose);
    // the escrow is the amount at the price; the rest is the delivery carriers (10 KAS each in this fixture), the fee and the reserve
    expect(40n * KAS - quoteOf(max, PRICE, CTX.scale, 'up')).toBeLessThan(25n * KAS);
  });

  it('is 0 when the balance cannot even cover the carriers', () => {
    expect(maxFor(ticketEnv({ funding: [1n * KAS] }), PRICE)).toBe(0n);
  });

  it('a ladder: the amount of one order such that all the orders together are affordable', () => {
    const env = ticketEnv({ funding: [200n * KAS] });
    const probe = limit(TOKEN);
    const lad = expandLimitLadder(probe, { levels: 5, mode: 'pct', bps: 200n }, 100n);
    const prices = lad.levels.map((l) => l.entryPrice);
    const max = maxFor(env, PRICE, prices);
    expect(max).toBeGreaterThan(0n);
    const cost = (n: bigint) => sum(prices.map((p) => spent(planOrder(env, limit(n, p)))));
    expect(cost(max) + MAX_KAS_RESERVE).toBeLessThanOrEqual(balanceOf(env));
    // one order alone could buy about five times as much
    expect(maxFor(env, PRICE)).toBeGreaterThan(max * 4n);
    // the next base unit of every order exceeds the budget by no more than the rounding of a few sompi
    expect(cost(max + 1000n) + MAX_KAS_RESERVE).toBeGreaterThan(balanceOf(env));
  });

  it('falls back to the rule of thumb where the planner cannot say', () => {
    const n = maxBuyAmount({ upper: 1_000n, conservative: 321n, kasBalance: 10n * KAS, planAt: () => null });
    expect(n).toBe(321n);
    const thrown = maxBuyAmount({ upper: 1_000n, conservative: 77n, kasBalance: 10n * KAS, planAt: () => { throw new Error('boom'); } });
    expect(thrown).toBe(77n);
  });
});

describe('max buy on a pair', () => {
  it('is bounded by the quote token balance, less a slack, and by the KAS for carriers', () => {
    const env = makePairEnv({ bAmounts: [30_000n] });
    const price = 1_400n;
    const bal = 30_000n;
    const kas = balanceOf(env);
    const scaleA = env.token.scale;
    const max = maxBuyAmount({
      upper: (bal * scaleA) / price + 1n,
      conservative: 0n,
      kasBalance: kas,
      quoteBalance: bal,
      planAt: (n) => planOrder(env, { type: 'limit', side: 'buy', amount: n, price }),
    });
    expect(max).toBeGreaterThan(0n);
    const at = planOrder(env, { type: 'limit', side: 'buy', amount: max, price });
    expect(errors(at)).toEqual([]);
    const over = planOrder(env, { type: 'limit', side: 'buy', amount: max + 1n, price });
    const escrow = (p: OrderPlan) => (p as unknown as { pair: { escrowB: bigint } }).pair.escrowB;
    expect(escrow(at) + MAX_QUOTE_SLACK).toBeLessThanOrEqual(bal);
    expect(!over.built || escrow(over) + MAX_QUOTE_SLACK > bal).toBe(true);
  });
});
