// Entry point of the order-planning layer, `planOrder(env, intent)`. Types are in plan-types.ts.
//
// A pair environment (a token/token pair A/B, see `isPairEnv`) is planned by `planPair`
// (orders/pair-plan.ts) from the same intents, with prices in B base units per whole A. Every other
// environment goes to the KAS planners, `planSimple` and `planCond`.
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
import { describeOrder, type OrderDescription } from './order-facts';
import { isPairEnv, type OrderPlan, type PlanEnv, type PlanIssue, type TokenMarket } from './plan-types';
import { longId } from './registry';
import { BPS, formatUnits } from './units';

/** Any order intent the planner accepts. */
export type Intent = SimpleIntent | CondIntent;

/** True for a conditional or if-done intent (it goes to `planCond` or the pair planner). */
export const isCondIntent = (i: Intent): i is CondIntent => COND_TYPES.includes(i.type);

/**
 * How urgent a placement is, for the fee policy (kob/fee-policy.ts).
 *
 * HIGH is an order that acts on the market now and that a keeper kills soon after it rests. These
 * are IOC, FOK, market, streaming and close (IOC auctions), and a limit that already crosses the
 * book, because a marketable limit becomes an auction. NORMAL is everything that rests. These are
 * GTC, GTD and day limits, TWAP, DCA and Dutch schedules, stops, take-profit, OCO, if-done,
 * bracket and repeat.
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
      // A limit that crosses the book now and is not refused is placed as an auction or fills from
      // its limit, so it trades now. A timed one rests first.
      // A pair order's tip is KAS, so it never moves its B price toward the book.
      return !intent.activeFrom && intent.crossing !== 'reject' && crossingTouch(env.book, intent.side, intent.price, env.pair ? 0n : intent.tip ?? 0n).crossing ? 'high' : 'normal';
    default:
      return 'normal';
  }
}

/**
 * Plans any order type. It never throws for user-input problems, which come back as `issues` with
 * severity 'error'. A pair environment returns a `PairOrderPlan` with `pair` (a `PairDisclosure`)
 * and, for conditional and if-done intents, `cond`.
 */
export function planOrder(env0: PlanEnv, intent: Intent): OrderPlan {
  const env = withUrgency(env0, orderUrgency(env0, intent));
  const plan = isPairEnv(env) ? planPair(env, intent) : isCondIntent(intent) ? planCond(env, intent) : planSimple(env, intent);
  const extra = [...envIssues(env), ...marketStartIssues(env, intent, plan), ...stopEvidenceIssues(env, plan)];
  if (!extra.length) return plan;
  return { ...plan, issues: [...plan.issues, ...extra], ok: plan.ok && !extra.some((i) => i.severity === 'error') };
}

/**
 * True when an order, or the exit an if-done entry commits, has a stop leg or a stop entry. Trigger
 * evidence arms or trails such a stop.
 */
function hasEvidenceStop(d: OrderDescription | null | undefined): boolean {
  if (!d) return false;
  return (d.trigger?.stopPrice ?? 0n) > 0n || (d.entry?.entryStop ?? 0n) > 0n || hasEvidenceStop(d.entry?.exit);
}

/**
 * Warns about stops whose trigger evidence a contaminated token genesis could fake.
 *
 * The stop-trigger evidence of the conditional and if-done covenants (KAS and pair) compares the
 * evidence order's token covenant id, template, scale and KCC-20 extension commitment. No covenant
 * can see units of that commitment that were minted outside the token's program. They cost their
 * holder nothing, so trades of them at any price the holder chooses can arm or trail a stop.
 *
 * A verified registry genesis (`genesis_verified: true`) rules that out. Every other KCC-20 token
 * gets the warning on any plan that places a stop leg, a trailing stop or a stop entry, including
 * one inside a committed exit.
 */
export function stopEvidenceIssues(env: PlanEnv, plan: Pick<OrderPlan, 'states'>): PlanIssue[] {
  if (!plan.states.some((s) => hasEvidenceStop(describeOrder(s, env.kob)))) return [];
  const tokens: TokenMarket[] = [env.token, ...(isPairEnv(env) ? [env.pair.quote] : [])];
  return tokens
    .filter((t) => t.family === 'kcc20' && t.genesisVerified !== true)
    .map((t) => issue('STOP_EVIDENCE_GENESIS_UNVERIFIED', { token: t.ticker && t.ticker !== longId(t.covenantId) ? `${t.ticker} (${longId(t.covenantId)})` : longId(t.covenantId) }));
}

/** The book price and the newest fill may differ by this many bps before the plan is flagged. */
export const REFERENCE_DIVERGENCE_BPS = 5_000n;

/**
 * Findings about the environment itself, added to every plan.
 *
 * These are guards whose data could not be read, which is an error because the order waits for the
 * data that the ticket's background refresh reads again, and a book reference that disagrees with
 * the last fill. One lying data path cannot move the reference unnoticed.
 */
export function envIssues(env: PlanEnv): PlanIssue[] {
  const out: PlanIssue[] = [];
  if (env.guardsUnavailable?.length) {
    out.push(issue('GUARDS_UNAVAILABLE', { guards: env.guardsUnavailable.join(', ') }));
  }
  const ref = referencePrice(env.book);
  const last = env.lastFillPrice ?? null;
  if (ref !== null && ref > 0n && last !== null && last > 0n) {
    const diff = ref > last ? ref - last : last - ref;
    const bps = (diff * BPS) / last;
    if (bps >= REFERENCE_DIVERGENCE_BPS) {
      const percent = formatUnits(bps, 2, { maxFraction: 1 });
      // A pair compares the pair book (B per whole A) with the rate the last KAS trades of A and B
      // imply, because pair fills never make a price.
      out.push(isPairEnv(env)
        ? pairIssue('PAIR_MARKET_REFERENCE_DIVERGES', { reference: ref, lastFill: last, percent, ticker: env.pair.quote.ticker })
        : issue('MARKET_REFERENCE_DIVERGES', { reference: ref, lastFill: last, percent }));
    }
  }
  return out;
}

/**
 * Default of `PlanEnv.marketStartToleranceBps` (config `marketStartToleranceBps`).
 *
 * A market or close auction that starts more than this on the costly side of an independent
 * reference cannot be placed.
 */
export const MARKET_START_TOLERANCE_BPS = 1_000n;

/**
 * Checks the start price of a market or close order against every further indexer.
 *
 * Such an order is an auction that starts at the best price of the indexer order book. The orders
 * at that touch were checked against the node when the book was read (app/book-verify.ts), so a
 * price nobody holds is not a start. The start is also compared with the best price of every
 * further indexer (`referenceTouches`, config `extraIndexerUrls`). A start more than the tolerance
 * on the costly side of one (above it for a buy, below it for a sell) is an error,
 * MARKET_START_VS_INDEXER, and the order cannot be placed.
 *
 * The last fill comes from the same indexer as the book, so it is no independent reference.
 * Without a further indexer the start is accepted.
 */
export function marketStartIssues(env: PlanEnv, intent: Intent, plan: OrderPlan): PlanIssue[] {
  if (intent.type !== 'market' && intent.type !== 'close') return [];
  const d = plan.disclosure;
  const start = d?.expectedPrice ?? null;
  if (!d || start === null || start <= 0n) return [];
  const buy = d.side === 'buy';
  const tol = env.marketStartToleranceBps ?? MARKET_START_TOLERANCE_BPS;
  const out: PlanIssue[] = [];
  // A further indexer configured as a reference did not answer. This is a warning, and the start is
  // checked without it.
  if (env.referencesUnavailable?.length) out.push(issue('MARKET_REFERENCE_INDEXER_UNAVAILABLE', { others: env.referencesUnavailable.join(', ') }));
  // The costliest gap is how far the start is beyond each reference on the side that costs the
  // user, in bps of the reference.
  let worst: { reference: bigint; other: string; bps: bigint } | null = null;
  for (const r of env.referenceTouches ?? []) {
    const reference = buy ? r.bestAsk : r.bestBid;
    if (reference === null || reference <= 0n) continue;
    const gap = buy ? start - reference : reference - start;
    if (gap <= 0n) continue;
    const bps = (gap * BPS) / reference;
    if (!worst || bps > worst.bps) worst = { reference, other: r.label, bps };
  }
  if (worst && worst.bps > tol) {
    out.push(issue('MARKET_START_VS_INDEXER', { start, reference: worst.reference, percent: formatUnits(worst.bps, 2, { maxFraction: 1 }), direction: buy ? 'above' : 'below', bps: worst.bps, other: worst.other }));
  }
  return out;
}
