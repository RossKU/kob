// Entry point of the order-planning layer: `planOrder(env, intent)`. See plan-types.ts.
//
// A PairPlanEnv (a token/token pair A/B: `isPairEnv`) is planned by `planPair` (orders/pair-plan.ts, KobPair / KobCondPair / KobIfdPair) from the
// same intents, prices in B base units per whole A; every other env by the KAS planners (planSimple / planCond).
import type { CondIntent } from './intent-cond';
import { COND_TYPES } from './intent-cond';
import type { SimpleIntent } from './intent-simple';
import { planCond } from './orders/cond';
import { planSimple } from './orders/simple';
import { withUrgency, type Urgency } from './fee-policy';
import { crossingTouch, referencePrice } from './guards';
import { issue } from './orders/common-issues';
import { planPair } from './orders/pair-plan';
import { pairIssue } from './orders/pair-issues';
import { isPairEnv, type OrderPlan, type PlanEnv, type PlanIssue } from './plan-types';
import { BPS, formatUnits } from './units';

export type Intent = SimpleIntent | CondIntent;

export const isCondIntent = (i: Intent): i is CondIntent => COND_TYPES.includes(i.type);

/**
 * How urgent a placement is, for the fee policy (kob/fee-policy.ts). HIGH: orders that act on the market now and that a keeper kills about a minute
 * after they rest: IOC / FOK, market, streaming and close (IOC auctions), and a limit that already crosses the book (a marketable limit becomes an
 * auction). NORMAL: everything that rests (GTC / GTD / day limits, TWAP / DCA / Dutch schedules, stops, take-profit, OCO, if-done / bracket, repeat).
 */
export function orderUrgency(env: Pick<PlanEnv, 'book'> & { pair?: unknown }, intent: Intent): Urgency {
  switch (intent.type) {
    case 'ioc':
    case 'fok':
    case 'market':
    case 'streaming':
    case 'close':
      return 'high';
    case 'limit':
      // a limit that crosses the book now and is not refused is placed as an auction (or fills from its limit): it trades now. A timed one rests first.
      // a pair order's tip is KAS: it never moves its B price toward the book
      return !intent.activeFrom && intent.crossing !== 'reject' && crossingTouch(env.book, intent.side, intent.price, env.pair ? 0n : intent.tip ?? 0n).crossing ? 'high' : 'normal';
    default:
      return 'normal';
  }
}

/**
 * Plans any order type. Never throws for user-input problems: they come back as `issues` (severity 'error'). A PairPlanEnv returns a
 * PairOrderPlan (`pair`: PairDisclosure; conditional and if-done intents also `cond`).
 */
export function planOrder(env0: PlanEnv, intent: Intent): OrderPlan {
  const env = withUrgency(env0, orderUrgency(env0, intent));
  const plan = isPairEnv(env) ? planPair(env, intent) : isCondIntent(intent) ? planCond(env, intent) : planSimple(env, intent);
  const extra = envIssues(env);
  if (!extra.length) return plan;
  return { ...plan, issues: [...plan.issues, ...extra], ok: plan.ok && !extra.some((i) => i.severity === 'error') };
}

/** A book price and the newest fill this far apart (bps) are flagged: one lying data path cannot move the reference unnoticed. */
export const REFERENCE_DIVERGENCE_BPS = 5_000n;

/**
 * Findings about the environment itself, added to every plan: guards that could not run (an error until acknowledged) and a book
 * reference that disagrees with the last fill.
 */
export function envIssues(env: PlanEnv): PlanIssue[] {
  const out: PlanIssue[] = [];
  if (env.guardsUnavailable?.length) {
    out.push(issue(env.guardsAcknowledged ? 'GUARDS_UNAVAILABLE_ACKNOWLEDGED' : 'GUARDS_UNAVAILABLE', { guards: env.guardsUnavailable.join(', ') }));
  }
  const ref = referencePrice(env.book);
  const last = env.lastFillPrice ?? null;
  if (ref !== null && ref > 0n && last !== null && last > 0n) {
    const diff = ref > last ? ref - last : last - ref;
    const bps = (diff * BPS) / last;
    if (bps >= REFERENCE_DIVERGENCE_BPS) {
      const percent = formatUnits(bps, 2, { maxFraction: 1 });
      // a pair: the pair book (B per whole A) against the rate the last KAS trades of A and B imply (pair fills never make a price)
      out.push(isPairEnv(env)
        ? pairIssue('PAIR_MARKET_REFERENCE_DIVERGES', { reference: ref, lastFill: last, percent, ticker: env.pair.quote.ticker })
        : issue('MARKET_REFERENCE_DIVERGES', { reference: ref, lastFill: last, percent }));
    }
  }
  return out;
}
