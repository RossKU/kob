// What the Portfolio's table cells show of a number and of a token's name (review M-03, H-05, L-05). Pure.
//  - an amount: at most 4 fraction digits, trailing zeros trimmed, the full value in the cell's title; a positive amount that would read zero says `<0.0001`; no subscript zeros
//  - a price column: ONE number of decimals for the whole column (from the finest price in it, `priceDpOf`), so the decimal points line up; the exact price in the title
//  - a token without a name of its own: its short id (`8347…cd64`) in the muted unit slot, never the long hex as a unit
import { formatUnits, statePriceToTokenPrice } from '../../kob/units';
import { longId, shortId as covenantFragment } from '../../kob/registry';
import { columnPriceDp, formatRowTime, shortId } from '../kit/format';

/** Most fraction digits an amount shows in a table or a strip; the title carries the rest. */
export const AMOUNT_FRACTION_MAX = 4;

export interface AmountText {
  text: string;
  /** the whole amount (every digit, grouped), when `text` is not it */
  title: string | undefined;
}

/** An amount of base units of `decimals`: at most 4 fraction digits (half up, trailing zeros trimmed), grouped; the full value as `title` when it is more. */
export function amountCell(value: bigint, decimals: number): AmountText {
  const full = formatUnits(value, decimals, { group: ',' });
  let text = formatUnits(value, decimals, { maxFraction: AMOUNT_FRACTION_MAX, group: ',' });
  // something that is there never reads as nothing
  if (value > 0n && /^0$/.test(text) && decimals > AMOUNT_FRACTION_MAX) text = `<0.${'0'.repeat(AMOUNT_FRACTION_MAX - 1)}1`;
  return { text, title: text === full ? undefined : full };
}

/** True where a token's `ticker` is only its covenant id's fragment (a token nobody named: app/token-display.ts shows it as 8 + 8 hex). */
export function isUnnamed(ticker: string | undefined | null, covenantId: string | null | undefined): boolean {
  if (!covenantId) return !ticker;
  return !ticker || ticker === longId(covenantId) || ticker === covenantFragment(covenantId) || ticker === shortId(covenantId) || ticker === covenantId;
}

export interface UnitText {
  text: string;
  /** the whole covenant id of an unnamed token (the unit's tooltip) */
  title: string | undefined;
}

/** The unit of a token's amount in a table: its ticker; without one, the short id of its covenant id (`8347…cd64`, the full id as the title). */
export function tokenUnit(ticker: string | undefined | null, covenantId: string | null | undefined): UnitText {
  if (isUnnamed(ticker, covenantId)) return covenantId ? { text: covenantFragment(covenantId), title: covenantId } : { text: '', title: undefined };
  return { text: ticker as string, title: undefined };
}

/**
 * The fixed number of decimals of a column of KAS-per-token prices: the finest any of the rows needs (kit `columnPriceDp`, 5 significant digits of each price, at most 6). `prices`: the
 * rows' state prices with the decimals and scale of their token (a price in sompi per `scale` base units); a row without a price or a scale is left out.
 */
export function priceDpOf(prices: Iterable<{ price: bigint | null | undefined; decimals: number; scale: bigint }>): number {
  const perToken: (bigint | null)[] = [];
  for (const p of prices) {
    if (p.price === null || p.price === undefined || p.price <= 0n || p.scale <= 0n) continue;
    perToken.push(statePriceToTokenPrice(p.price, p.decimals, p.scale, 'nearest'));
  }
  return columnPriceDp(perToken);
}


/**
 * A date inside a sentence ("renew before ...", "repeats until ..."): the table's 24-hour form with the day always spelled, `2026-11-10 22:35`, in the viewer's zone. (`formatRowTime` shows
 * today as a bare clock; a sentence about a date must name the day, so it is asked with a clock that is never the same day.)
 */
export const longDate = (unix: number | bigint): string => formatRowTime(Number(unix) * 1000, 0);
