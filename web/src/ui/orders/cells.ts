// What the Portfolio's table cells show of a number and of a token's name. This module is pure.
// - An amount has at most 4 fraction digits and trailing zeros trimmed. The full value is in the
//   cell's title. A positive amount that would read zero says `<0.0001`. There are no subscript
//   zeros.
// - A price column has one number of decimals for the whole column, from the finest price in it
//   (`priceDpOf`), so the decimal points line up. The exact price is in the title.
// - A token without a name of its own shows its short id (`8347…cd64`) in the muted unit slot,
//   never the long hex as a unit.
import { formatUnits, statePriceToTokenPrice } from '../../kob/units';
import { longId, shortId as covenantFragment, tickerNeedsId } from '../../kob/registry';
import { columnPriceDp, formatRowTime, shortId } from '../kit/format';

/** Most fraction digits an amount shows in a table or a strip. The title carries the rest. */
export const AMOUNT_FRACTION_MAX = 4;

export interface AmountText {
  text: string;
  /** The whole amount, with every digit and grouped, when `text` is not it. */
  title: string | undefined;
}

/**
 * An amount of base units of `decimals`. It has at most 4 fraction digits, rounded half up with
 * trailing zeros trimmed, and is grouped. The full value is the `title` when it is more.
 */
export function amountCell(value: bigint, decimals: number): AmountText {
  const full = formatUnits(value, decimals, { group: ',' });
  let text = formatUnits(value, decimals, { maxFraction: AMOUNT_FRACTION_MAX, group: ',' });
  // Something that is there never reads as nothing.
  if (value > 0n && /^0$/.test(text) && decimals > AMOUNT_FRACTION_MAX) text = `<0.${'0'.repeat(AMOUNT_FRACTION_MAX - 1)}1`;
  return { text, title: text === full ? undefined : full };
}

/**
 * True where a token's `ticker` is only its covenant id's fragment. This is a token nobody named,
 * which app/token-display.ts shows as 8 + 8 hex.
 */
export function isUnnamed(ticker: string | undefined | null, covenantId: string | null | undefined): boolean {
  if (!covenantId) return !ticker;
  return !ticker || ticker === longId(covenantId) || ticker === covenantFragment(covenantId) || ticker === shortId(covenantId) || ticker === covenantId;
}

export interface UnitText {
  text: string;
  /** The whole covenant id of an unnamed token, as the unit's tooltip. */
  title: string | undefined;
}

/**
 * The unit of a token's amount in a table. It is its ticker, or without one the short id of its
 * covenant id (`8347…cd64`) with the full id as the title. A ticker that reads as KAS or is not
 * plain ASCII (`tickerNeedsId`) carries that short id too (`KAS (8347…cd64)`), so a token amount
 * never reads as KAS. The short form keeps the table's columns as they are; the title has the id.
 */
export function tokenUnit(ticker: string | undefined | null, covenantId: string | null | undefined): UnitText {
  if (isUnnamed(ticker, covenantId)) return covenantId ? { text: covenantFragment(covenantId), title: covenantId } : { text: '', title: undefined };
  if (covenantId && tickerNeedsId(ticker)) return { text: `${ticker as string} (${covenantFragment(covenantId)})`, title: covenantId };
  return { text: ticker as string, title: undefined };
}

/**
 * The fixed number of decimals of a column of KAS-per-token prices. It is the finest any of the
 * rows needs (kit `columnPriceDp`, 5 significant digits of each price, at most 6). `prices` are the
 * rows' state prices with the decimals and scale of their token, where a price is in sompi per
 * `scale` base units. A row without a price or a scale is left out.
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
 * A date inside a sentence ("renew before ...", "repeats until ..."). It is the table's 24-hour
 * form with the day always spelled, as `YYYY-MM-DD HH:MM`, in the viewer's zone. `formatRowTime`
 * shows today as a bare clock, but a sentence about a date must name the day. So it is asked with a
 * clock that is never the same day.
 */
export const longDate = (unix: number | bigint): string => formatRowTime(Number(unix) * 1000, 0);
