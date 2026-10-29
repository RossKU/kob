// The planner's warnings the ticket shows as one-line notes next to the order button. They never stop the order and need no click (the order
// goes from the button straight to the wallet, 4c318276); only errors disable the button. Pure and DOM-free.
import { isKind } from '../../kob/order-facts';
import type { OrderPlan, PlanIssue } from '../../kob/plan-types';
import type { OrderState } from '../../kob/types';
import { quoteOf } from '../../kob/units';

/**
 * What the ticket notes: a price far through the market (a typing error), a market reference that disagrees with the last fill, book orders
 * the node does not hold (left out of the price the order starts from), a high slippage, a
 * day order about to end, a limit that crosses the market (it fills at the limit, the difference goes to the matcher), the conditional orders'
 * market checks (every `COND_*` warning, and the pair planner's take-profit twin) and a stop on a token whose genesis was not verified. The pair
 * planner's own spellings of the price and reference codes are the same warnings on a pair.
 */
const NOTE_CODES: ReadonlySet<string> = new Set([
  'PRICE_AGGRESSIVE_VS_MARKET', 'MARKET_REFERENCE_DIVERGES', 'SLIPPAGE_HIGH', 'DAY_ORDER_ENDS_SOON', 'MARKETABLE_LIMIT_FILLS_AT_LIMIT', 'STOP_EVIDENCE_GENESIS_UNVERIFIED',
  'PAIR_PRICE_AGGRESSIVE_VS_MARKET', 'PAIR_MARKET_REFERENCE_DIVERGES', 'PAIR_MARKETABLE_LIMIT_FILLS_AT_LIMIT', 'PAIR_TP_CROSSES', 'PAIR_TP_NOT_PROFITABLE',
]);

/** A warning of the plan that the ticket notes (an error is never a note: it is the red line that disables the button). */
export const isTicketNote = (i: Pick<PlanIssue, 'code' | 'severity'>): boolean =>
  i.severity === 'warning' && (NOTE_CODES.has(i.code) || i.code.startsWith('COND_'));

/** The notes of a plan's findings, in the planner's order, one per code and wording (an entry and an exit leg can raise the same one). */
export function ticketNotes(issues: readonly PlanIssue[]): PlanIssue[] {
  const seen = new Set<string>();
  const out: PlanIssue[] = [];
  for (const i of issues) {
    if (!isTicketNote(i)) continue;
    const k = `${i.code}|${i.message}`;
    if (seen.has(k)) continue;
    seen.add(k);
    out.push(i);
  }
  return out;
}

// ------------------------------------------------------------------------------------------------ minimum order value

/**
 * The executor's default minimum order value, in sompi (1 KAS): `ListingRules::min_order_value_sompi` in crates/kob-executor/src/tokens.rs. An order
 * worth less is indexed (status, cancel) but never listed (`unlisted_reason: "order_value_below_minimum"`), so it is in no book and never fills.
 * Each operator can set another value under `[rules]` of the executor config; `/v1/health` reports the executor's own (`min_order_value_sompi`,
 * docs/api.md section 3.1) and the web app follows it, assuming this default only for an executor that does not (see {@link minOrderValueOf}).
 */
export const DEFAULT_MIN_ORDER_VALUE_SOMPI = 100_000_000n;

/**
 * The minimum order value to warn at: the indexer's, when `/v1/health` reports it (`min_order_value_sompi`, sompi as a decimal string or a safe
 * integer; executors from 0.1.0 do), else the executor default. `source` says which one it is, so the note can say that the number is only the default.
 */
export function minOrderValueOf(health: unknown): { sompi: bigint; source: 'indexer' | 'default' } {
  const v = health && typeof health === 'object' ? (health as { min_order_value_sompi?: unknown }).min_order_value_sompi : undefined;
  if (typeof v === 'string' && /^\d+$/.test(v)) return { sompi: BigInt(v), source: 'indexer' };
  if (typeof v === 'number' && Number.isSafeInteger(v) && v >= 0) return { sompi: BigInt(v), source: 'indexer' };
  return { sompi: DEFAULT_MIN_ORDER_VALUE_SOMPI, source: 'default' };
}

const bigOrNull = (v: unknown): bigint | null => {
  try {
    return typeof v === 'string' || typeof v === 'number' || typeof v === 'bigint' ? BigInt(v) : null;
  } catch {
    return null;
  }
};

/**
 * The value the executor's listing rule measures for a plain KAS order (kob-executor `ListingRules::evaluate`): a sell's amount at its quote,
 * `quoteOf(amountLeft, price, scale)` rounded down (kob-protocol `quote_exact`); a bid's KAS escrow (`carrier`: the whole value of its order UTXO).
 * `null` when the rule does not measure it here (a conditional, stop, if-done or pair order) or the state is not readable.
 */
export function listingValueOf(state: OrderState, carrier: bigint): bigint | null {
  if (isKind(state, 'KobBid')) return carrier >= 0n ? carrier : null;
  if (!isKind(state, 'KobAsk')) return null;
  const s = state.state;
  const [amount, price, scale] = [bigOrNull(s.amountLeft), bigOrNull(s.price), bigOrNull(s.scale)];
  if (amount === null || price === null || scale === null || amount < 0n || price < 0n || scale <= 0n) return null;
  return quoteOf(amount, price, scale, 'down');
}

/** The order's value and the minimum it falls short of. */
export interface MinValueNote {
  value: bigint;
  min: bigint;
}

/**
 * The note for an order worth less than the minimum (`value < min`: a value equal to it is listed), from a plan of ONE plain order; `null` when the
 * order is worth the minimum or more, or its value is not known (no plan, a conditional / stop / pair order, several orders). Never an error: the
 * order can still be placed, and cancelled from My orders.
 */
export function minValueNote(plan: Pick<OrderPlan, 'states' | 'request'> | null, min: bigint): MinValueNote | null {
  if (!plan || plan.states.length !== 1 || !plan.request) return null;
  const carrier = bigOrNull(plan.request.value);
  if (carrier === null) return null;
  const value = listingValueOf(plan.states[0]!, carrier);
  return value !== null && value < min ? { value, min } : null;
}
