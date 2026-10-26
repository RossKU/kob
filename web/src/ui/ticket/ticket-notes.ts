// The planner's warnings the ticket shows as one-line notes next to the order button. They never stop the order and need no click (the order
// goes from the button straight to the wallet, 4c318276); only errors disable the button. Pure and DOM-free.
import type { PlanIssue } from '../../kob/plan-types';

/**
 * What the ticket notes: a price far through the market (a typing error), a market reference that disagrees with the last fill, a high slippage, a
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
