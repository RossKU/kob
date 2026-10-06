// The soak's side of the dynamic fee policy (web `kob/fee-policy.ts`, the same module the wallet uses): the planners get the policy and the node's
// estimate through their environment (market.ts), and the transactions the bots build without a planner (token consolidation, fan-out) go through
// `buildAtUrgency`. Every submitted transaction's urgency, rate and source (estimate / floor) is recorded by wallet.ts `recordTx`.
// relative (not `@/`): unit-tested by node --test, which has no path alias
import { atFloor, buildWithCap, readFeeContext, recordFee, withUrgency, type FeeContext, type FeeEnvFields, type FeeRequestLike, type Urgency } from '../../../web/src/kob/fee-policy.ts';
import type { BuiltTx } from '@/kob/types';
import type { Env } from './env';

/** The policy and the node's newest usable estimate (cached: at most one `getFeeEstimate` per refresh interval; never rejects). */
export const feeContext = (env: Env): Promise<FeeContext> => readFeeContext(env.fees.policy, env.fees.oracle);

/**
 * Builds `request` at the rate the policy picks for `urgency` (its `fee.feeRate`), with the total cap rebuild, and remembers the choice for the
 * transaction record. A build that fails at the picked rate (the carriers cannot pay the dearer fee) is retried once at the floor, so the estimate
 * never makes a payable transaction impossible.
 */
export async function buildAtUrgency<R extends object>(env: Env, urgency: Urgency, request: R, build: (r: R) => BuiltTx): Promise<BuiltTx> {
  const ctx = await feeContext(env);
  const e: FeeEnvFields = withUrgency({ fees: ctx } as FeeEnvFields, urgency);
  const at = (rate: bigint | undefined): R & FeeRequestLike => ({ ...request, ...(rate === undefined ? {} : { fee: { feeRate: rate.toString() } }) });
  try {
    const { built, rate } = buildWithCap(ctx.policy, at(e.feeRate), build);
    recordFee(e, built, rate);
    return built;
  } catch (err) {
    const low = atFloor(e);
    if (!low) throw err;
    const { built, rate } = buildWithCap(ctx.policy, at(low.feeRate), build);
    recordFee(low, built, rate);
    return built;
  }
}
