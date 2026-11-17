// The executor tip that makes an order fill at any spread (the ticket's "Tip executors" option).
//
// An executor fills an order only when what it earns (the spread and the tips) pays the match
// transaction's network fee. A spread thinner than that fee never fills. The tip here is the fee of
// a match of this very order, measured by building one now: the planned order (its placement's own
// outputs) matched one to one with an opposite order of the same price at the minimum fill. Times
// TIP_FEE_MARGIN_PCT, spread over the minimum fill, so every partial fill pays its own match.
import type { OrderPlan, PlanEnv } from './plan-types';
import type { KeyUtxo, OrderState, TokenUtxo } from './types';
import { baseKind } from './order-facts';
import { makeAsk, makeBid, str } from './orders/common';
import { custodyState, keyState } from './token-state';
import { ceilDiv } from './units';

/** The tip pays this percentage of the measured match fee. */
export const TIP_FEE_MARGIN_PCT = 125n;

const DUMMY = (n: number): string => n.toString(16).padStart(2, '0').repeat(32);

/**
 * The network fee of one match of the planned order (`plan.states[0]`, a plain KobAsk or KobBid of
 * a KAS market) at its minimum fill, built now at the plan's fee rate, in sompi. A one-order fill
 * when the match does not build; null when neither does.
 */
export function measureMatchFee(env: PlanEnv, plan: OrderPlan): bigint | null {
  const st = plan.states[0];
  const built = plan.built;
  if (!st || !built || plan.states.length !== 1) return null;
  const k = baseKind(st.kind);
  if (k !== 'KobAsk' && k !== 'KobBid') return null;
  let rec;
  try {
    rec = env.kob.recoverOrders(built.tx).find((r) => r.output === 0) ?? env.kob.recoverOrders(built.tx)[0];
  } catch {
    return null;
  }
  if (!rec) return null;
  const s = st.state as unknown as Record<string, string>;
  const n = BigInt(s.minFill!);
  const price = BigInt(s.price!);
  if (n <= 0n || price <= 0n) return null;
  const daa = env.clock.daa;
  const at = (BigInt(s.activeFrom ?? '0') > daa ? BigInt(s.activeFrom!) : daa).toString();
  const tx = built.tx.id;
  const own = { transactionId: tx, index: rec.output, amount: rec.value, blockDaaScore: str(daa), covenantId: rec.covenantId, state: st.state };
  const ownCustody: TokenUtxo | null = rec.custody
    ? { transactionId: tx, index: rec.custody.output, amount: rec.custody.value, blockDaaScore: str(daa), covenantId: env.token.covenantId, state: rec.custody.state }
    : null;
  const ownLeg = k === 'KobAsk' ? { kind: 'ask', order: own, custody: ownCustody, amount: str(n), t: null } : { kind: 'bid', order: own, amount: str(n), t: null };
  const fee = built.fee.feeRate ? { feeRate: built.fee.feeRate, ...(built.fee.feeMode ? { feeMode: built.fee.feeMode } : {}) } : undefined;
  const funding: KeyUtxo[] = [{ transactionId: DUMMY(0xf1), index: 0, amount: str(1_000_000n * 100_000_000n), blockDaaScore: str(daa), covenantId: null, pubkey: env.maker }];
  const base = { action: 'batch', lockTime: at, updates: [], receivers: [], payments: [], funding, change: env.maker, records: [], takerTokenCarrier: str(env.carrier ?? 100_000_000n), ...(fee ? { fee } : {}) };
  const feeOf = (r: unknown): bigint | null => {
    try {
      return BigInt(env.kob.build(r as never).fee.fee);
    } catch {
      return null;
    }
  };
  // the match: an opposite order of the same price at the minimum fill, its own UTXO
  const exp = BigInt(s.expiryDaa ?? '0');
  const common = { minFill: n, price, expiryDaa: exp };
  let other: OrderState;
  let otherLeg: unknown;
  if (k === 'KobAsk') {
    other = makeBid(env, common);
    const escrow = env.kob.bidEscrow(other, n, 1n);
    if (escrow === null) return null;
    otherLeg = { kind: 'bid', order: { transactionId: DUMMY(0xf2), index: 0, amount: str(escrow), blockDaaScore: str(daa), covenantId: DUMMY(0xf3), state: other.state }, amount: str(n), t: null };
  } else {
    other = makeAsk(env, { ...common, amount: n });
    const custody: TokenUtxo = { transactionId: DUMMY(0xf2), index: 1, amount: str(env.carrier ?? 100_000_000n), blockDaaScore: str(daa), covenantId: env.token.covenantId, state: custodyState(env.token.family, n, DUMMY(0xf3), env.token.family === 'kron' ? null : env.token.extensionCommitment) };
    otherLeg = { kind: 'ask', order: { transactionId: DUMMY(0xf2), index: 0, amount: str(env.carrier ?? 100_000_000n), blockDaaScore: str(daa), covenantId: DUMMY(0xf3), state: other.state }, custody, amount: str(n), t: null };
  }
  const match = feeOf({ ...base, legs: k === 'KobAsk' ? [otherLeg, ownLeg] : [ownLeg, otherLeg], takerTokens: [], taker: env.maker });
  if (match !== null) return match;
  // one order filled by a taker (an executor with inventory)
  const takerTokens: TokenUtxo[] = k === 'KobBid'
    ? [{ transactionId: DUMMY(0xf4), index: 0, amount: str(env.carrier ?? 100_000_000n), blockDaaScore: str(daa), covenantId: env.token.covenantId, state: keyState(env.token.family, n, env.maker, env.token.extensionCommitment, env.token.program) }]
    : [];
  return feeOf({ ...base, legs: [ownLeg], takerTokens, taker: env.maker });
}

/** Sompi per whole token that pay `fee` x TIP_FEE_MARGIN_PCT on every fill of at least `minFill` base units (rounded up). */
export function tipForFee(fee: bigint, minFill: bigint, scale: bigint): bigint {
  if (fee <= 0n || minFill <= 0n) return 0n;
  return ceilDiv(fee * TIP_FEE_MARGIN_PCT * scale, 100n * minFill);
}

/** The tip of the plan's order (sompi per whole token) and what it pays in all at the order's amount, or null when it cannot be measured. */
export function executorTip(env: PlanEnv, plan: OrderPlan): { tip: bigint; fee: bigint; total: bigint } | null {
  const fee = measureMatchFee(env, plan);
  const d = plan.disclosure;
  if (fee === null || !d || d.minFill <= 0n) return null;
  const tip = tipForFee(fee, d.minFill, d.scale);
  return { tip, fee, total: ceilDiv(d.tokenAmount * tip, d.scale) };
}
