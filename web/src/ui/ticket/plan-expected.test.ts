import { describe, expect, it } from 'vitest';
import { decodeSigning } from '../../kob/decode';
import { planOrder } from '../../kob/plan';
import { errors } from '../../kob/plan-types';
import { MAKER_PK, kob } from '../../testing/fixtures';
import { nodeFactsOf } from '../../testing/chain-fixtures';
import { buildIntent } from './form-state';
import { expectedFromPlan } from './plan-expected';
import { CASES, CTX, form, ticketEnv } from './ticket-fixtures';

const K = kob();

describe('the planner claim passes the pre-sign decoder for every order type', () => {
  for (const [name, type, side, values] of CASES) {
    it(name, () => {
      const intent = buildIntent(form(type, side, values), CTX).intent!;
      const plan = planOrder(ticketEnv(), intent);
      expect(errors(plan)).toEqual([]);
      const s = decodeSigning({ kob: K, built: plan.built!, maker: MAKER_PK, registry: null, expected: expectedFromPlan(plan), nodeInputs: nodeFactsOf(plan.built!) });
      // the unlisted-token warning is expected (no registry here); nothing may block
      expect(s.blocking.map((b) => `${b.code}: ${b.message}`)).toEqual([]);
      expect(s.ok).toBe(true);
      expect(s.kind).toBe('create');
      expect(s.orders.length).toBeGreaterThanOrEqual(1);
    });
  }

  it('a claim that differs from the transaction blocks', () => {
    const intent = buildIntent(form('limit', 'sell', { amount: '5', price: '2.6' }), CTX).intent!;
    const plan = planOrder(ticketEnv(), intent);
    const wrong = { ...expectedFromPlan(plan), kasLocked: expectedFromPlan(plan).kasLocked! + 1n };
    const s = decodeSigning({ kob: K, built: plan.built!, maker: MAKER_PK, registry: null, expected: wrong, nodeInputs: nodeFactsOf(plan.built!) });
    expect(s.blocking.map((b) => b.code)).toContain('expected-kas-locked');
  });
});
