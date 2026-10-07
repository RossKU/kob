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
import { isPairEnv, type MarketStartAck, type OrderPlan, type PlanEnv, type PlanIssue } from './plan-types';
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
  const extra = [...envIssues(env), ...marketStartIssues(env, intent, plan)];
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

/**
 * Default of `PlanEnv.marketStartToleranceBps` (config `marketStartToleranceBps`): a market / close auction that starts more than this on the costly
 * side of a reference is held until the user acknowledges it.
 */
export const MARKET_START_TOLERANCE_BPS = 1_000n;

/**
 * A market or close order is an auction that starts at the best price of the indexer order book. That book is not checked against the node, so the
 * start is compared with every reference the app has that is not the book itself: the last fill (the indexer trades) and the best price of each
 * further indexer (`referenceTouches`). A start more than the tolerance on the costly side (above a reference for a buy, below for a sell) is an
 * error until acknowledged. The last fill comes from the same indexer as the book, so it is not independent: with no further indexer answering, an
 * order worth `marketStartUnverifiedMinSompi` or more is an error until acknowledged (MARKET_START_UNVERIFIED), a smaller one a warning. An
 * acknowledgement (`marketStartAck`) covers the gap (or start) it was given for and nothing worse.
 */
export function marketStartIssues(env: PlanEnv, intent: Intent, plan: OrderPlan): PlanIssue[] {
  if (intent.type !== 'market' && intent.type !== 'close') return [];
  const d = plan.disclosure;
  const start = d?.expectedPrice ?? null;
  if (!d || start === null || start <= 0n) return [];
  const buy = d.side === 'buy';
  const direction: 'above' | 'below' = buy ? 'above' : 'below';
  const tol = env.marketStartToleranceBps ?? MARKET_START_TOLERANCE_BPS;
  const ack = env.marketStartAck ?? null;
  const out: PlanIssue[] = [];
  // a further indexer configured as a reference that did not answer is said, never dropped silently
  if (env.referencesUnavailable?.length) out.push(issue('MARKET_REFERENCE_INDEXER_UNAVAILABLE', { others: env.referencesUnavailable.join(', ') }));
  const refs: { reference: bigint; other: string | null }[] = [];
  if (env.lastFillPrice && env.lastFillPrice > 0n) refs.push({ reference: env.lastFillPrice, other: null });
  let independent = 0;
  for (const r of env.referenceTouches ?? []) {
    const p = buy ? r.bestAsk : r.bestBid;
    if (p !== null && p > 0n) {
      refs.push({ reference: p, other: r.label });
      independent += 1;
    }
  }
  // the costliest gap: how far the start is beyond each reference on the side that costs the user (bps of the reference)
  let worst: { reference: bigint; other: string | null; bps: bigint } | null = null;
  for (const r of refs) {
    const gap = buy ? start - r.reference : r.reference - start;
    if (gap <= 0n) continue;
    const bps = (gap * BPS) / r.reference;
    if (!worst || bps > worst.bps) worst = { ...r, bps };
  }
  if (worst && worst.bps > tol) {
    const params = { start, reference: worst.reference, percent: formatUnits(worst.bps, 2, { maxFraction: 1 }), direction, bps: worst.bps };
    // the acknowledgement covers the gap it was given for, or a smaller one in the same direction: a worse start is held again
    if (ack?.kind === 'gap' && ack.direction === direction && worst.bps <= ack.bps) out.push(issue('MARKET_START_ACKNOWLEDGED', params));
    else out.push(worst.other === null ? issue('MARKET_START_VS_LAST_FILL', params) : issue('MARKET_START_VS_INDEXER', { ...params, other: worst.other }));
    return out;
  }
  if (independent > 0) return out;
  // no independent reference: the last fill (if any) comes from the same indexer as the book, so it cannot catch a wrong book
  const value = startValueSompi(env, d.tokenAmount, start, d.scale);
  const min = env.marketStartUnverifiedMinSompi ?? MARKET_START_UNVERIFIED_MIN_SOMPI;
  if (value === null || value >= min) {
    const params = { start, direction, bps: 0n };
    const covered = ack?.kind === 'unverified' && ack.direction === direction && (buy ? start <= ack.start : start >= ack.start);
    out.push(issue(covered ? 'MARKET_START_UNVERIFIED_ACKNOWLEDGED' : 'MARKET_START_UNVERIFIED', params));
  } else {
    out.push(refs.length ? issue('MARKET_START_NO_INDEPENDENT_REFERENCE', { start }) : issue('MARKET_START_UNCHECKED', { start }));
  }
  return out;
}

/**
 * Default of `PlanEnv.marketStartUnverifiedMinSompi`: a market / close order worth at least this much (100 KAS) at its start is held until
 * acknowledged when no independent reference checks the start.
 */
export const MARKET_START_UNVERIFIED_MIN_SOMPI = 100n * 100_000_000n;

/** The KAS value (sompi) of `amount` base units at `start`; a pair converts B at its KAS reference. Null when unknown. */
function startValueSompi(env: PlanEnv, amount: bigint, start: bigint, scale: bigint): bigint | null {
  if (scale <= 0n) return null;
  const quote = (amount * start) / scale;
  if (!isPairEnv(env)) return quote;
  const kasPerWholeB = env.pair.kasPerWholeB;
  const scaleB = env.pair.quote.scale;
  return kasPerWholeB === null || kasPerWholeB <= 0n || scaleB <= 0n ? null : (quote * kasPerWholeB) / scaleB;
}

/** The acknowledgement the user gives by ticking the box under `finding` (a MARKET_START_* error), or null for any other issue. */
export function marketStartAckOf(finding: PlanIssue): MarketStartAck | null {
  const p = finding.params ?? {};
  const direction = p.direction === 'below' ? 'below' : 'above';
  const start = typeof p.start === 'bigint' ? p.start : null;
  const bps = typeof p.bps === 'bigint' ? p.bps : null;
  if (start === null || bps === null) return null;
  if (finding.code === 'MARKET_START_VS_LAST_FILL' || finding.code === 'MARKET_START_VS_INDEXER') return { kind: 'gap', direction, bps, start };
  if (finding.code === 'MARKET_START_UNVERIFIED') return { kind: 'unverified', direction, bps, start };
  return null;
}
