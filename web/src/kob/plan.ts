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
import { describeOrder, type OrderDescription } from './order-facts';
import { isPairEnv, type OrderPlan, type PlanEnv, type PlanIssue, type TokenMarket } from './plan-types';
import { longId } from './registry';
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
  const extra = [...envIssues(env), ...bookCheckIssues(env, intent), ...marketStartIssues(env, intent, plan), ...stopEvidenceIssues(env, plan)];
  if (!extra.length) return plan;
  return { ...plan, issues: [...plan.issues, ...extra], ok: plan.ok && !extra.some((i) => i.severity === 'error') };
}

/** True when an order (or the exit an if-done entry commits) has a stop that trigger evidence arms or trails: a stop leg or a stop entry. */
function hasEvidenceStop(d: OrderDescription | null | undefined): boolean {
  if (!d) return false;
  return (d.trigger?.stopPrice ?? 0n) > 0n || (d.entry?.entryStop ?? 0n) > 0n || hasEvidenceStop(d.entry?.exit);
}

/**
 * The stop-trigger evidence of the conditional / if-done covenants (KAS and pair) compares the evidence order's token covenant id, template, scale and
 * KCC-20 extension commitment (since the order template version after the audit 2026-10, finding AUD-EXT), but no covenant can see units of that
 * commitment minted outside the token's program by a contaminated genesis: they cost their holder nothing, so their trades, at any price the holder
 * chooses, can arm or trail a stop. The registry's genesis check (`genesis_verified: true`) rules that out; every other KCC-20 token (all open-list
 * tokens) gets this warning on any plan that places a stop leg, a trailing stop or a stop entry (also inside a committed exit); KCC-20 tokens only,
 * as before.
 */
export function stopEvidenceIssues(env: PlanEnv, plan: Pick<OrderPlan, 'states'>): PlanIssue[] {
  if (!plan.states.some((s) => hasEvidenceStop(describeOrder(s, env.kob)))) return [];
  const tokens: TokenMarket[] = [env.token, ...(isPairEnv(env) ? [env.pair.quote] : [])];
  return tokens
    .filter((t) => t.family === 'kcc20' && t.genesisVerified !== true)
    .map((t) => issue('STOP_EVIDENCE_GENESIS_UNVERIFIED', { token: t.ticker && t.ticker !== longId(t.covenantId) ? `${t.ticker} (${longId(t.covenantId)})` : longId(t.covenantId) }));
}

/**
 * Orders of the book the node did not confirm (`env.bookDropped`, app/book-verify.ts) that an order taking liquidity now would have met: the order
 * is priced against the best order the node does hold, and says so (a warning, one line; nothing to acknowledge). Orders of the side the order itself
 * adds to are of no concern to it. An order that rests (a limit away from the market, a stop ...) takes nothing and says nothing.
 */
export function bookCheckIssues(env: PlanEnv, intent: Intent): PlanIssue[] {
  if (!env.bookDropped?.length || orderUrgency(env, intent) !== 'high') return [];
  const side = intent.type === 'close' ? 'sell' : 'side' in intent ? intent.side : undefined;
  // the user buys from asks (the orders that sell) and sells to bids
  const met = env.bookDropped.filter((d) => side === undefined || (side === 'buy' ? d.side === 'sell' : d.side === 'buy'));
  if (!met.length) return [];
  const asks = met.some((d) => d.side === 'sell');
  const prices = met.filter((d) => (asks ? d.side === 'sell' : d.side === 'buy') && d.price > 0n).map((d) => d.price);
  const best = prices.length ? prices.reduce((a, b) => ((asks ? b < a : b > a) ? b : a)) : 0n;
  return [issue('BOOK_ORDERS_NOT_ON_NODE', { count: met.length, side: asks ? 'ask' : 'bid', price: best })];
}

/** A book price and the newest fill this far apart (bps) are flagged: one lying data path cannot move the reference unnoticed. */
export const REFERENCE_DIVERGENCE_BPS = 5_000n;

/**
 * Findings about the environment itself, added to every plan: guards whose data could not be read (an error: the order waits for the data, which the
 * ticket's background refresh reads again) and a book reference that disagrees with the last fill.
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
 * side of an independent reference cannot be placed.
 */
export const MARKET_START_TOLERANCE_BPS = 1_000n;

/**
 * A market or close order is an auction that starts at the best price of the indexer order book. The orders at its touch were checked against the node
 * when the book was read (app/book-verify.ts), so a price nobody holds is not a start; the start is also compared with the best price of every further indexer (`referenceTouches`, config `extraIndexerUrls`): a start more than the tolerance
 * on the costly side of one (above it for a buy, below for a sell) is an error (MARKET_START_VS_INDEXER), the order cannot be placed at that start.
 * The last fill comes from the same indexer as the book, so it is no independent reference: without a further indexer the start is accepted.
 */
export function marketStartIssues(env: PlanEnv, intent: Intent, plan: OrderPlan): PlanIssue[] {
  if (intent.type !== 'market' && intent.type !== 'close') return [];
  const d = plan.disclosure;
  const start = d?.expectedPrice ?? null;
  if (!d || start === null || start <= 0n) return [];
  const buy = d.side === 'buy';
  const tol = env.marketStartToleranceBps ?? MARKET_START_TOLERANCE_BPS;
  const out: PlanIssue[] = [];
  // a further indexer configured as a reference that did not answer (a warning: the start is checked without it)
  if (env.referencesUnavailable?.length) out.push(issue('MARKET_REFERENCE_INDEXER_UNAVAILABLE', { others: env.referencesUnavailable.join(', ') }));
  // the costliest gap: how far the start is beyond each reference on the side that costs the user (bps of the reference)
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
