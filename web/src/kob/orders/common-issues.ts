// Stable issue codes of the order planners (plan-types.ts `PlanIssue.code`), with their default severity and English fallback text.
//
// The UI translates by `code` (i18n key `plan.issue.<CODE>`) and fills the placeholders from `params`; `message` is only the fallback.
// Placeholders: `{name}` inserts params.name, `{name:kas}` formats a sompi bigint as KAS, `{name:bps}` a basis-point value as a percent.
// Conditional-order codes (stops, OCO, if-done, repeat) live in the conditional planner's own catalogue; this file holds the codes of the
// shared layer (guards, funding, building) and of the simple orders.
import type { IssueSeverity, PlanIssue } from '../plan-types';
import { formatBps, formatKas } from '../units';

interface CatalogEntry { severity: IssueSeverity; message: string }
const e = (severity: IssueSeverity, message: string): CatalogEntry => ({ severity, message });

export const ISSUE_CATALOG = {
  // ---- intent shape
  INTENT_UNKNOWN_TYPE: e('error', 'Unknown order type "{type}".'),
  SIDE_INVALID: e('error', 'This order type cannot be placed on the {side} side.'),
  AMOUNT_NOT_POSITIVE: e('error', 'Enter an amount greater than zero.'),
  AMOUNT_TOO_LARGE: e('error', 'The amount is too large for one order.'),
  MIN_FILL_INVALID: e('error', 'The minimum fill must be at least one base unit and at most the order amount.'),
  NOTIONAL_TOO_LARGE: e('error', 'The order value at this price is too large for the protocol (it must stay below 2^62 sompi).'),
  PRICE_NOT_POSITIVE: e('error', 'The price must be greater than zero.'),
  PRICE_TOO_LARGE: e('error', 'The price is too large.'),
  PRICE_NOT_ON_TICK: e('error', 'The price must be a multiple of the tick ({tick} sompi per token); nearest valid prices: {below} / {above}.'),
  PRICE_END_INVALID: e('error', 'The end price must be {direction} the start price.'),
  TIP_NEGATIVE: e('error', 'The priority tip cannot be negative.'),
  TIP_TOO_LARGE: e('error', 'The priority tip is too large for this quantity.'),
  TIP_EXCEEDS_PRICE: e('error', 'The priority tip ({tip:kas} per token) must be smaller than the price.'),
  DURATION_INVALID: e('error', 'The duration must be positive.'),
  LIFE_OUT_OF_RANGE: e('error', 'The order life must be between 1 and {max} DAA (the covenant kills the order after {max} DAA).'),
  SLIPPAGE_INVALID: e('error', 'The slippage tolerance must be between 0.01% and 99.99%.'),
  SLIPPAGE_HIGH: e('warning', 'A slippage tolerance of {bps:bps}% lets the order fill far from the market price.'),
  REFERENCE_PRICE_INVALID: e('error', 'The displayed price is not valid.'),
  MAX_FILLS_INVALID: e('error', 'The number of fills must be a positive integer.'),
  // ---- time
  EXPIRY_TOO_SOON: e('error', 'The expiry must be later than the moment the order becomes active.'),
  EXPIRY_TOO_FAR: e('error', 'An order can live at most 90 days.'),
  ACTIVE_TOO_FAR: e('error', 'The activation time must be within 90 days.'),
  ACTIVE_AFTER_EXPIRY: e('error', 'The order would expire before it activates.'),
  ACTIVE_IN_PAST: e('info', 'The activation time has passed: the order activates immediately.'),
  DAY_ORDER_ENDS_SOON: e('warning', 'This day order ends at 00:00 UTC in {minutes} minutes.'),
  // ---- TWAP / DCA / decay
  SLICE_AMOUNT_INVALID: e('error', 'The amount per slice must be greater than zero.'),
  TWAP_EXCEEDS_LIFE: e('error', 'The schedule ({slices} slices every {intervalDaa} DAA) does not fit into the order life.'),
  TWAP_SINGLE_SLICE: e('info', 'Every slice is larger than the whole order: it is placed as one slice.'),
  // ---- market and book
  NO_LIQUIDITY: e('error', 'There is no {counterparty} in the book to price a market order against.'),
  MARKET_DEPTH_INSUFFICIENT: e('warning', 'The visible book within your slippage bound holds only {available} of your {amount} base units; the rest is returned unfilled.'),
  MARKETABLE_AUCTION: e('info', 'Your limit crosses the market: it is placed as an auction from the best price ({touch:kas}) down to your limit.'),
  MARKETABLE_LIMIT_FILLS_AT_LIMIT: e('warning', 'Your limit crosses the market and fills at your limit price; a matcher keeps the difference to the better price ({touch:kas}).'),
  MARKETABLE_REJECTED: e('error', 'Your limit crosses the market at {touch:kas}.'),
  PRICE_AGGRESSIVE_VS_MARKET: e('warning', 'Your price is {percent}% through the market ({reference:kas}): check for a typing error.'),
  PRICE_FAR_FROM_MARKET: e('warning', 'Your price is {percent}% away from the market ({reference:kas}): it is unlikely to fill soon.'),
  // ---- guards
  GUARDS_UNAVAILABLE: e('error', 'The safety checks that need the indexer ({guards}) could not run, so a self-trade or an unfillable order would not be caught. Acknowledge this to place the order anyway.'),
  GUARDS_UNAVAILABLE_ACKNOWLEDGED: e('warning', 'You placed this order without the safety checks that need the indexer ({guards}).'),
  MARKET_REFERENCE_DIVERGES: e('warning', 'The order book shows {reference:kas} per token, but the last fill was at {lastFill:kas} ({percent}% apart). Both come from the indexer: check the price yourself before signing.'),
  MARKET_REFERENCE_SOURCE: e('info', 'Market reference: {reference:kas} per token from the indexer order book. The order can fill at {worst:kas} per token at worst.'),
  SELF_TRADE: e('error', 'This order would trade against your own resting order {ownCovenantId}. Cancel it first or change the price.'),
  FOK_INSUFFICIENT_DEPTH: e('error', 'Fill-or-kill needs {amount} base units but only {available} cross at your price: it could not fill.'),
  FOK_TOO_MANY_COUNTERPARTIES: e('error', 'Filling in one transaction would need {count} counterparties, more than the token allows ({max}): use IOC or a smaller size.'),
  FOK_TOO_MANY_TOKEN_INPUTS: e('error', 'Filling in one transaction would need {count} token inputs, more than the 8 a transaction may carry.'),
  CARRIERS_DOMINATE: e('warning', 'The KAS locked as carriers ({locked:kas}) exceeds the order value; it is returned when the order ends, but this order is very small.'),
  // ---- funding
  INSUFFICIENT_KAS: e('error', 'Not enough KAS: this order needs {needed:kas}, your spendable balance is {have:kas} (short by {shortfall:kas}).'),
  INSUFFICIENT_TOKENS: e('error', 'Not enough tokens: this order needs {needed} base units, you hold {have} (short by {shortfall}).'),
  TOKEN_UTXOS_FRAGMENTED: e('error', 'Your tokens are spread over too many UTXOs: one transaction can spend at most {max}. Merge them first (send them to yourself).'),
  FUNDING_FRAGMENTED: e('error', 'Your KAS is spread over too many UTXOs: one transaction can spend at most {max}. Consolidate them first.'),
  CLOSE_NOTHING_TO_SELL: e('error', 'You hold none of this token: there is nothing to close.'),
  CARRIER_BELOW_FLOOR: e('error', 'The KAS carrier ({carrier:kas}) is below {floor:kas}, the least this token output can carry (the floor of the token program or the dust bound of the network).'),
  // ---- building
  BUILD_REJECTED: e('error', 'The protocol refused this order: {reason}'),
  PLAN_MISMATCH: e('error', 'The built transaction does not match the planned order ({reason}): not signing.'),
} as const satisfies Record<string, CatalogEntry>;

export type IssueCode = keyof typeof ISSUE_CATALOG;
export type IssueParams = Record<string, string | number | bigint>;

function interpolate(template: string, params: IssueParams | undefined): string {
  return template.replace(/\{(\w+)(?::(\w+))?\}/g, (whole, name: string, fmt?: string) => {
    const v = params?.[name];
    if (v === undefined) return whole;
    if (fmt === 'kas' && typeof v === 'bigint') return `${formatKas(v)} KAS`;
    if (fmt === 'bps' && typeof v === 'bigint') return formatBps(v);
    return String(v);
  });
}

/** Creates a PlanIssue of the catalogue: default severity and English text, `params` for the placeholders, `field` for inline errors. */
export function issue(code: IssueCode, params?: IssueParams, field?: string, severity?: IssueSeverity): PlanIssue {
  const c: CatalogEntry = ISSUE_CATALOG[code];
  const out: PlanIssue = { code, severity: severity ?? c.severity, message: interpolate(c.message, params) };
  if (params) out.params = params;
  if (field) out.field = field;
  return out;
}

/** Free-form issue for codes outside the catalogue (the conditional planner's own catalogue). */
export function customIssue(code: string, severity: IssueSeverity, message: string, params?: IssueParams, field?: string): PlanIssue {
  const out: PlanIssue = { code, severity, message };
  if (params) out.params = params;
  if (field) out.field = field;
  return out;
}

export const hasError = (issues: readonly PlanIssue[]): boolean => issues.some((i) => i.severity === 'error');
