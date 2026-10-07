// Test helpers of the conditional-order planner tests (not imported by production code): run a plan through the full oracle chain
// (built -> local signatures -> finalize with tightened budgets -> script-engine validate -> placement records recovered from the tx).
import { expect } from 'vitest';
import type { PlanEnv } from '../plan-types';
import type { CondIntent } from '../intent-cond';
import type { AnyState, OrderKind, CondAskState, CondBidState, IfdAskState, IfdBidState, OrderState, RecoveredOrder, SignedTx } from '../types';
import { consensusCheck } from '../../testing/fixtures';
import { baseKind, familyOfKind, kindFor } from '../order-facts';
import { planCond } from './cond';
import type { CondPlan } from './cond-common';

export const sompi = (kas: string): bigint => {
  const [w, f = ''] = kas.split('.');
  return BigInt(w!) * 100_000_000n + BigInt(f.padEnd(8, '0'));
};

export const b = (s: string): bigint => BigInt(s);

export interface Checked {
  plan: CondPlan;
  signed: SignedTx;
  recovered: RecoveredOrder[];
}

/** Plans, expects success and consensus validity, and cross-checks the placement record against the planned state. */
export function planOk(env: PlanEnv, intent: CondIntent): Checked {
  const plan = planCond(env, intent);
  expect(plan.issues.filter((i) => i.severity === 'error'), JSON.stringify(plan.issues, (_k, v) => (typeof v === 'bigint' ? v.toString() : v))).toEqual([]);
  expect(plan.ok).toBe(true);
  expect(plan.built).not.toBeNull();
  const signed = consensusCheck(env.kob, plan.built!);
  const recovered = env.kob.recoverOrders(signed.tx);
  expect(recovered).toHaveLength(1);
  // the placement record re-derives exactly the planned (entry) order and its value
  expect(env.kob.encodeState(recovered[0]!.order)).toBe(env.kob.encodeState(plan.states[0]!));
  expect(BigInt(recovered[0]!.value)).toBe(BigInt(plan.built!.tx.outputs[0]!.value));
  // KAS conservation of the built transaction: what goes in comes out plus the fee
  const tx = plan.built!.tx;
  const inSum = tx.inputs.reduce((a, i) => a + BigInt(i.utxo.amount), 0n);
  const outSum = tx.outputs.reduce((a, o) => a + BigInt(o.value), 0n);
  expect(inSum).toBe(outSum + BigInt(plan.built!.fee.fee));
  // what the disclosure says is locked is what sits on the order UTXO and its custody token UTXO
  const locked = BigInt(recovered[0]!.value) + (recovered[0]!.custody ? BigInt(recovered[0]!.custody.value) : 0n);
  expect(plan.disclosure!.kasLocked).toBe(locked);
  return { plan, signed, recovered };
}

export const cond = (p: CondPlan, i = 0): OrderState => p.states[i]!;
export const askState = (s: OrderState | AnyState): CondAskState => {
  expect(baseKind(s.kind as OrderKind)).toBe('KobCondAsk');
  return s.state as CondAskState;
};
export const bidState = (s: OrderState | AnyState): CondBidState => {
  expect(baseKind(s.kind as OrderKind)).toBe('KobCondBid');
  return s.state as CondBidState;
};
export const ifdBid = (s: OrderState | AnyState): IfdBidState => {
  expect(baseKind(s.kind as OrderKind)).toBe('KobIfdBid');
  return s.state as IfdBidState;
};
export const ifdAsk = (s: OrderState | AnyState): IfdAskState => {
  expect(baseKind(s.kind as OrderKind)).toBe('KobIfdAsk');
  return s.state as IfdAskState;
};

/** codes of the issues of a plan, in order */
export const codes = (p: CondPlan): string[] => p.issues.map((i) => i.code);
export const errorCodes = (p: CondPlan): string[] => p.issues.filter((i) => i.severity === 'error').map((i) => i.code);

/**
 * The plain exit an if-done entry commits to, decoded through kob-wasm exactly like `IfdBidState::exit()` / `IfdAskState::exit()`:
 * the committed prefix followed by the zero repeat fields decodes as the exit's state.
 */
export function committedExit(env: PlanEnv, entry: OrderState): AnyState {
  const family = familyOfKind(entry.kind);
  if (baseKind(entry.kind) === 'KobIfdBid') {
    // the zero repeat fields, then (KCC-20) the entry's extensionCommitment, which it writes behind them
    const e = entry.state as IfdBidState;
    const tail = '20' + '00'.repeat(32) + '08' + '00'.repeat(8) + '08' + '00'.repeat(8) + (family === 'kron' ? '' : '20' + e.extensionCommitment);
    return env.kob.decodeState(kindFor('KobCondAsk', family), e.exitState + tail);
  }
  if (baseKind(entry.kind) === 'KobIfdAsk') {
    const tail = '20' + '00'.repeat(32) + '08' + '00'.repeat(8) + '08' + '00'.repeat(8) + '08' + '00'.repeat(8);
    return env.kob.decodeState(kindFor('KobCondBid', family), (entry.state as IfdAskState).exitState + tail);
  }
  throw new Error('not an if-done entry');
}
