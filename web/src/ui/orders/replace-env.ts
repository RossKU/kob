// The planning side of a replacement (cancel-replace from the full ticket form), without the
// dialog. It holds the environment the replacement is planned in and the ReplacementSpec of the
// planned order. ReplaceDialog uses both, and the TN10 soak's pair amend (tools/soak) runs the same
// code. This module is pure.
import type { ReplacementSpec } from '../../kob/cancel';
import { custodiesOf } from '../../kob/order-facts';
import { isPairEnv, type OrderPlan, type PlanEnv } from '../../kob/plan-types';
import { isPlainState, keyState } from '../../kob/token-state';
import type { Hex, OrderState } from '../../kob/types';

/**
 * What `replaceEnv` reads of the replaced order's row: its id and the KAS on its current UTXO. An
 * `OrderRowModel` fits.
 */
export interface ReplacedOrderLike {
  id: Hex;
  entry: {
    view?: { current?: { value?: string | null } | null } | null;
    resolved?: { order?: { amount?: string | null } | null } | null;
  };
}

const SYNTH_TXID = 'ff'.repeat(32);

/**
 * The planning environment of a replacement. The order's own custody tokens and KAS come back in
 * the same transaction, so the planner may count them. They are stand-in UTXOs it never spends,
 * because only the planned order state, value and carriers are kept and `planCancelReplace` builds
 * the real transaction. The order itself is no own order to self-trade against. A pair order's
 * custodies come back per token, with A into the base UTXOs and B into the quote ones. Only plain
 * wallet UTXOs count next to them. A top-up joins the plain custody, so one with a borrow setting
 * cannot (kob/cancel.ts `pickTokens`).
 */
export function replaceEnv(env: PlanEnv, row: ReplacedOrderLike, state: OrderState): PlanEnv {
  const amount = row.entry.view?.current?.value ?? row.entry.resolved?.order?.amount ?? '0';
  const custodies = custodiesOf(state);
  const heldOf = (token: Hex): bigint => custodies.filter((c) => c.token === token).reduce((a, c) => a + c.amount, 0n);
  const synth = (market: PlanEnv['token'], held: bigint, index: number) => ({
    transactionId: SYNTH_TXID, index, amount: '100000000', covenantId: market.covenantId,
    state: keyState(market.family, held, env.maker, market.family === 'kron' ? null : market.extensionCommitment),
  });
  const heldA = heldOf(env.token.covenantId);
  const plain = <U extends { state: Parameters<typeof isPlainState>[0] }>(us: readonly U[]): U[] => us.filter((u) => isPlainState(u.state));
  const out: PlanEnv = {
    ...env,
    ownOrders: env.ownOrders.filter((o) => o.covenantId !== row.id),
    funding: [...env.funding, { transactionId: SYNTH_TXID, index: 0, amount: String(amount), pubkey: env.maker }],
    tokenUtxos: heldA > 0n ? [...plain(env.tokenUtxos), synth(env.token, heldA, 1)] : env.tokenUtxos,
  };
  if (isPairEnv(env)) {
    const heldB = heldOf(env.pair.quote.covenantId);
    return { ...out, pair: { ...env.pair, quoteTokenUtxos: heldB > 0n ? [...plain(env.pair.quoteTokenUtxos), synth(env.pair.quote, heldB, 2)] : env.pair.quoteTokenUtxos } } as PlanEnv;
  }
  return out;
}

/** The replacement spec of a planned order, which is its createOrder request. */
export function specOfPlan(p: OrderPlan): ReplacementSpec | null {
  const r = p.request;
  if (!r) return null;
  return { order: r.order, value: BigInt(r.value), tokenCarrier: r.tokenCarrier != null ? BigInt(r.tokenCarrier) : null, deadline: r.deadline != null ? BigInt(r.deadline) : null };
}
