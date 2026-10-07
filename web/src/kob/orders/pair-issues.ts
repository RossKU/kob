// Issue codes of the pair planner (token/token pairs A/B, kob/orders/pair-*.ts), same conventions as common-issues.ts: the UI translates by
// `code` (i18n `plan.issue.<CODE>` / `issues.<CODE>`), `message` is the English fallback, `params` fill the placeholders.
//
// A pair order reuses the shared codes whose meaning and units are the same (SELF_TRADE, AMOUNT_*, PRICE_NOT_POSITIVE, PRICE_TOO_LARGE,
// TIP_NEGATIVE, TIP_TOO_LARGE (the tip is KAS), EXPIRY_*, ACTIVE_*, FOK_*, NO_LIQUIDITY, MARKET_DEPTH_INSUFFICIENT, SLIPPAGE_*, INSUFFICIENT_KAS,
// GUARDS_UNAVAILABLE*, BUILD_REJECTED, PLAN_MISMATCH and the conditional codes COND_* without a KAS price). The codes below exist because their
// numbers are B base units per whole A (never KAS) or because the finding only exists on a pair. Prices in params are B base units per whole A
// (`price` raw integers, the UI formats them with B's decimals: `ticker` names B where a sentence needs it), amounts base units.
import type { IssueSeverity, PlanIssue } from '../plan-types';
import type { IssueParams } from './common-issues';

interface CatalogEntry { severity: IssueSeverity; message: string }
const e = (severity: IssueSeverity, message: string): CatalogEntry => ({ severity, message });

export const PAIR_ISSUE_CATALOG = {
  PAIR_SAME_TOKEN: e('error', 'A pair needs two different tokens.'),
  PAIR_NOTIONAL_TOO_LARGE: e('error', 'The order value at this price is too large for the protocol (it must stay below 2^62 base units of {ticker}).'),
  PAIR_MARKETABLE_AUCTION: e('info', 'Your limit crosses the pair book: it is placed as an auction from the best price ({touch} {ticker} per token) to your limit.'),
  PAIR_MARKETABLE_LIMIT_FILLS_AT_LIMIT: e('warning', 'Your limit crosses the pair book and fills at your limit price; a filler keeps the difference to the better price ({touch} {ticker} per token).'),
  PAIR_MARKETABLE_REJECTED: e('error', 'Your limit crosses the pair book at {touch} {ticker} per token.'),
  PAIR_PRICE_AGGRESSIVE_VS_MARKET: e('warning', 'Your price is {percent}% through the pair market ({reference} {ticker} per token): check for a typing error.'),
  PAIR_PRICE_FAR_FROM_MARKET: e('warning', 'Your price is {percent}% away from the pair market ({reference} {ticker} per token): it is unlikely to fill soon.'),
  PAIR_MARKET_REFERENCE_SOURCE: e('info', 'Market reference: {reference} {ticker} per token from the indexer pair book (resting pair orders and the route through the two KAS books). The order can fill at {worst} {ticker} per token at worst.'),
  PAIR_MARKET_REFERENCE_DIVERGES: e('warning', 'The pair book shows {reference} {ticker} per token, but the last KAS trades of the two tokens imply {lastFill} ({percent}% apart). Both come from the indexer: check the price yourself before signing.'),
  PAIR_INSUFFICIENT_TOKENS: e('error', 'Not enough {ticker}: this order needs {needed} base units, you hold {have} (short by {shortfall}).'),
  PAIR_TOKEN_UTXOS_FRAGMENTED: e('error', 'Your {ticker} is spread over too many UTXOs: one transaction can spend at most {max}. Merge them first (send them to yourself).'),
  PAIR_KRON_CUSTODY_TOO_LARGE: e('error', 'The order would hold {amount} base units of {ticker} in one custody, more than a KRON token output can hold ({max}): lower the amount or the price.'),
  PAIR_KRON_DELIVERY_TOO_LARGE: e('error', 'One minimum fill would deliver more {ticker} than a KRON token output can hold ({max} base units): lower the minimum fill or the price.'),
  PAIR_TP_CROSSES: e('warning', 'The take-profit crosses the pair book ({touch} {ticker} per token): it fills right away at your limit.'),
  PAIR_TP_NOT_PROFITABLE: e('error', 'The take-profit does not beat the entry price: the profit per token would be {profitPerToken} base units of {ticker}.'),
  PAIR_PREFUND_SHORT: e('error', 'The prefund does not cover the exit\'s worst buy-back: at least {needed} base units of {ticker} per token are needed (you gave {given}).'),
  PAIR_FILLS_LIMITED: e('info', 'The order funds {fills} deliveries: after {partials} partial fills, what is left fills only in full. A larger minimum fill or more fills changes this.'),
  PAIR_KAS_REFERENCE_MISSING: e('info', 'The KAS value of {ticker} is unknown: the default minimum fill is a quarter of the amount.'),
} as const satisfies Record<string, CatalogEntry>;

export type PairIssueCode = keyof typeof PAIR_ISSUE_CATALOG;
export const PAIR_ISSUE_CODES = Object.keys(PAIR_ISSUE_CATALOG) as PairIssueCode[];

function interpolate(template: string, params: IssueParams | undefined): string {
  return template.replace(/\{(\w+)(?::(\w+))?\}/g, (whole, name: string) => {
    const v = params?.[name];
    return v === undefined ? whole : String(v);
  });
}

/** Creates a PlanIssue of the pair catalogue (default severity unless overridden). */
export function pairIssue(code: PairIssueCode, params?: IssueParams, field?: string, severity?: IssueSeverity): PlanIssue {
  const c: CatalogEntry = PAIR_ISSUE_CATALOG[code];
  const out: PlanIssue = { code, severity: severity ?? c.severity, message: interpolate(c.message, params) };
  if (params) out.params = params;
  if (field) out.field = field;
  return out;
}
