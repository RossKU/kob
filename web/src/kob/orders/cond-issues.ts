// Issue codes of the conditional / if-done planner (stops, take-profit, OCO, trailing, IFD / IFO, repeat), same conventions as
// common-issues.ts: the UI translates by `code` (i18n `plan.issue.<CODE>`), `message` is the English fallback, `params` fill the
// placeholders `{name}`, `{name:kas}` (sompi bigint -> KAS) and `{name:bps}`. Codes shared with the simple orders (SELF_TRADE,
// INSUFFICIENT_KAS, PRICE_NOT_ON_TICK, AMOUNT_NOT_POSITIVE, EXPIRY_TOO_FAR, ...) stay in common-issues.ts and are reused as they are.
import type { IssueSeverity, PlanIssue } from '../plan-types';
import { formatBps, formatKas } from '../units';
import type { IssueParams } from './common-issues';

interface CatalogEntry { severity: IssueSeverity; message: string }
const e = (severity: IssueSeverity, message: string): CatalogEntry => ({ severity, message });

export const COND_ISSUE_CATALOG = {
  // ---- legs
  COND_LEGS_MISSING: e('error', 'A conditional order needs a take-profit or a stop.'),

  COND_TP_STOP_ORDER: e('error', 'The take-profit must be {direction} the stop.'),
  COND_STOP_LIMIT_BEYOND_STOP: e('error', 'A {side} stop-limit price must not be {direction} the stop.'),
  COND_SLIP_INVALID: e('error', 'The slippage band must be a whole number of basis points between 0 and 10000.'),
  COND_BAND_INVALID: e('error', 'The auction length must not be negative.'),
  COND_WORST_PRICE_INVALID: e('error', 'The stop band reaches a price of zero: use a smaller slippage band.'),
  COND_MIN_REST_INVALID: e('error', 'The trigger exposure must not be negative.'),
  COND_MIN_TOUCH_INVALID: e('error', 'The trigger threshold must be at least one base unit.'),
  COND_KEEPER_TIP_INVALID: e('error', 'The keeper tip must not be negative.'),
  COND_KEEPER_FUNDING_TOO_LARGE: e('error', 'The pre-funded keeper tips ({reserve:kas}) do not fit the carrier: raise the carrier or lower the tip / update count.'),
  COND_CARRIER_INVALID: e('error', 'The carrier must be positive.'),
  // ---- trailing
  COND_TRAIL_NEEDS_STOP: e('error', 'A trailing order needs a stop.'),
  COND_TRAIL_STEP_INVALID: e('error', 'The trailing step must be positive.'),
  COND_TRAIL_GAP_INVALID: e('error', 'The trailing gap must be zero or positive and a whole number of price units.'),
  COND_TRAIL_WAIT_TOO_SHORT: e('error', 'The trailing update interval must be at least {min} DAA (60 s).'),
  COND_TRAIL_UPDATES_INVALID: e('error', 'Pre-funded trailing updates must be a whole number from 0 to {max}.'),
  // ---- market checks (warnings)
  COND_STOP_ALREADY_REACHED: e('warning', 'The market is already {direction} your stop: it arms as soon as a qualifying trade is proven.'),
  COND_TP_CROSSES: e('warning', 'The take-profit crosses the market ({touch:kas}): it fills right away at your limit.'),
  // ---- if-done
  COND_EXIT_NEEDS_ONE_LEG: e('error', 'An IFD exit is exactly one leg: a take-profit or a stop (use IFO for both).'),
  COND_EXIT_NEEDS_TP_ONLY: e('error', 'A repeat IFD exit is the take-profit only (add a stop with a repeat IFO).'),
  COND_EXIT_NEEDS_BOTH: e('error', 'An IFO exit needs both a take-profit and a stop.'),
  COND_EXIT_LIMIT_NEEDS_STOP: e('error', 'A stop-limit exit needs its stop.'),
  COND_EXIT_EXPIRY_DAY: e('error', 'An exit is created after the entry fills: it cannot be a day order.'),
  COND_ENTRY_STOP_BEYOND_LIMIT: e('error', 'A {side} stop entry must trigger at or {direction} its limit price.'),
  COND_ENTRY_CROSSES: e('warning', 'The entry price is at or beyond the market: it fills right away at your limit.'),
  COND_TP_NOT_PROFITABLE: e('error', 'The take-profit does not beat the entry (all-in, tips included): the profit per token would be {profitPerToken:kas}.'),
  COND_MIN_FILL_INVALID: e('error', 'The smallest entry fill must be between 1 and {amount} base units.'),
  COND_PREFUND_INVALID: e('error', 'The prefund must not be negative.'),
  COND_PREFUND_SHORT: e('error', 'The prefund does not cover the exit\'s worst buy-back: at least {needed:kas} per token are needed (you gave {given:kas}).'),
  COND_REPEAT_COUNT_INVALID: e('error', 'The repeat count must be at least 1 (use a plain IFD for no repeat).'),
} as const satisfies Record<string, CatalogEntry>;

export type CondIssueCode = keyof typeof COND_ISSUE_CATALOG;
export const COND_ISSUE_CODES = Object.keys(COND_ISSUE_CATALOG) as CondIssueCode[];

function interpolate(template: string, params: IssueParams | undefined): string {
  return template.replace(/\{(\w+)(?::(\w+))?\}/g, (whole, name: string, fmt?: string) => {
    const v = params?.[name];
    if (v === undefined) return whole;
    if (fmt === 'kas' && typeof v === 'bigint') return `${formatKas(v)} KAS`;
    if (fmt === 'bps' && typeof v === 'bigint') return formatBps(v);
    return String(v);
  });
}

/**
 * Creates a PlanIssue of the conditional catalogue. Codes whose text depends on the side (`{rule}`) get the sentence as a param, so
 * the UI can still translate by `code` and rebuild the sentence from the numbers in the other params.
 */
export function condIssue(code: CondIssueCode, params?: IssueParams, field?: string, severity?: IssueSeverity): PlanIssue {
  const c: CatalogEntry = COND_ISSUE_CATALOG[code];
  const out: PlanIssue = { code, severity: severity ?? c.severity, message: interpolate(c.message, params) };
  if (params) out.params = params;
  if (field) out.field = field;
  return out;
}
