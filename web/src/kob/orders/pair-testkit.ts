// Oracle of the pair planner tests: a plan must be ok, consensus-valid (signed locally, finalized with tightened budgets, run by the script
// engine), its placement record recovered by kob-wasm must be the planned state, every custody it creates must be an output of its own token
// owned by the new order at exactly the state's amount, and the KAS / token accounting of the built transaction must reconcile with the
// disclosure.
import { expect } from 'vitest';
import type { Intent } from '../plan';
import { planOrder } from '../plan';
import type { PairOrderPlan, PairPlanEnv } from '../plan-types';
import { errors } from '../plan-types';
import type { CondSummary } from './cond-common';
import { consensusCheck, kob } from '../../testing/fixtures';

export const K = kob();
const sum = (xs: bigint[]): bigint => xs.reduce((a, b) => a + b, 0n);

export type TestPairPlan = PairOrderPlan & { cond?: CondSummary | null };

export const plan = (env: PairPlanEnv, intent: unknown): TestPairPlan => planOrder(env, intent as Intent) as TestPairPlan;
export const codes = (p: { issues: { code: string }[] }): string[] => p.issues.map((i) => i.code);
export const errorCodes = (p: { issues: { code: string; severity: string }[] }): string[] => p.issues.filter((i) => i.severity === 'error').map((i) => i.code);

/** Plans and runs every oracle; returns the plan. */
export function pairValid(env: PairPlanEnv, intent: unknown): TestPairPlan {
  const p = plan(env, intent);
  expect(errors(p), JSON.stringify(errors(p), (_k, v) => (typeof v === 'bigint' ? v.toString() : v))).toEqual([]);
  expect(p.ok).toBe(true);
  const built = p.built!;
  const d = p.disclosure!;
  const x = p.pair!;
  const order = p.states[0];
  consensusCheck(K, built);
  K.checkNumbers(order);
  K.checkNewOrder(order);

  // the placement record (kob-wasm recoverOrders trusts nothing) is the planned order, at the planned value
  const rec = K.recoverOrders(built.tx);
  expect(rec).toHaveLength(1);
  expect(rec[0].order).toEqual(order);
  expect(BigInt(rec[0].value)).toBe(x.orderValue);
  expect(BigInt(built.tx.outputs[0].value)).toBe(x.orderValue);
  expect(x.orderValue >= K.minOrderValue(order)).toBe(true);

  // every custody of the state is created at its exact amount, owned by the new order id, as an output of ITS token
  const custodies = K.custodies(order);
  const parts = [rec[0].custody, rec[0].prefund ?? null];
  for (let i = 0; i < 2; i++) {
    const c = custodies[i];
    if (!c) {
      expect(parts[i]).toBeNull();
      continue;
    }
    expect(BigInt(parts[i]!.state.amount)).toBe(c.amount);
    expect(parts[i]!.state.owner).toBe(rec[0].covenantId);
    expect(built.tx.outputs[parts[i]!.output].covenant?.covenantId).toBe(c.token);
  }
  const a = env.token.covenantId;
  expect(x.escrowA).toBe(sum(custodies.filter((c) => c.token === a).map((c) => c.amount)));
  expect(x.escrowB).toBe(sum(custodies.filter((c) => c.token !== a).map((c) => c.amount)));
  expect(d.tokensEscrowed).toBe(x.escrowA);

  // token conservation per token: what the maker's token inputs hold = custody + change (each output of its token)
  for (const tok of [env.token.covenantId, env.pair.quote.covenantId]) {
    const inputs = p.request!.tokens?.filter((u) => u.covenantId === tok) ?? [];
    const held = sum(inputs.map((u) => BigInt(u.state.amount)));
    const cust = sum(custodies.filter((c) => c.token === tok).map((c) => c.amount));
    if (cust === 0n) expect(inputs).toHaveLength(0);
    else expect(held >= cust).toBe(true);
  }

  // KAS accounting: inputs = outputs + fee; kasLocked = inputs - change - fee - the wallet-owned token-change carriers
  const inSum = sum(built.tx.inputs.map((i) => BigInt(i.utxo.amount)));
  const outSum = sum(built.tx.outputs.map((o) => BigInt(o.value)));
  const fee = BigInt(built.fee.fee);
  expect(inSum - outSum).toBe(fee);
  expect(d.fee).toBe(fee);
  const change = built.fee.changeOutput == null ? 0n : BigInt(built.tx.outputs[built.fee.changeOutput].value);
  const kept = sum(d.carriers.filter((c) => c.kept).map((c) => c.amount * BigInt(c.count)));
  expect(d.kasLocked).toBe(sum(d.carriers.filter((c) => !c.kept).map((c) => c.amount * BigInt(c.count))));
  expect(d.kasLocked).toBe(inSum - change - fee - kept);
  // the order UTXO lines (everything but the token carriers) sum to the order value
  const orderLines = d.carriers.filter((c) => !/[tT]okenCarrier$|[tT]okenChangeCarrier$/.test(c.kind));
  expect(sum(orderLines.map((c) => c.amount * BigInt(c.count)))).toBe(x.orderValue);

  // the generic disclosure in the pair meaning: the all-in price is the limit (the tip is KAS), the scale is A's
  expect(d.allInPrice).toBe(d.limitPrice);
  expect(d.scale).toBe(env.token.scale);
  expect(d.minFill).toBe(BigInt((order.state as { minFill: string }).minFill));
  expect(x.base.covenantId).toBe(env.token.covenantId);
  expect(x.quote.covenantId).toBe(env.pair.quote.covenantId);
  expect(x.notes).toContain('pairPricesFromKasBooks');
  return p;
}
