// Whether an order entered in the ticket would be dust in the public book (pure), and the one
// flash of the ticket's "?" that says so.
//
// kobd lists an order or a TWAP / DCA slice that offers a taker less than half the listing minimum
// to a taker right now (one fill's amount at its price) apart, as dust: executors still fill it,
// but `/v1/books` and `/v1/depth` leave it out by default (docs/ops/api-bounds.md, the book rule).
// The ticket says so in the help of what decides the size, and its "?" flashes once when the order
// being entered crosses into that range. The check is the simple, generous side of kobd's: it
// values one fill at the order's best price (an auction's start or end, whichever is higher) and
// leaves out the carriers, so an order it calls dust is dust for kobd too. Pair orders have no
// dust range, and kinds that do not rest (market, IOC, FOK, close) are never in a book.
import { MIN_ORDER_VALUE_SOMPI } from '../../data/stream-book';
import type { Intent } from '../../kob/plan';

/** Below this, in sompi, one fill of an order is dust: half the listing minimum. */
export const DUST_LINE_SOMPI = MIN_ORDER_VALUE_SOMPI / 2n;

/**
 * True when `intent` would rest as dust: a limit order whose amount, or a TWAP / DCA order whose
 * slice, is worth more than nothing and less than `line` at its price. Anything unsure is false.
 */
export function wouldBeDust(intent: Intent | null | undefined, scale: bigint, line: bigint = DUST_LINE_SOMPI): boolean {
  if (!intent || scale <= 0n) return false;
  const i = intent as { type: string; price?: bigint; priceEnd?: bigint; amount?: bigint; sliceAmount?: bigint };
  if (typeof i.price !== 'bigint' || typeof i.amount !== 'bigint' || i.price <= 0n || i.amount <= 0n) return false;
  let fill: bigint;
  if (i.type === 'limit') fill = i.amount;
  else if (i.type === 'twap' || i.type === 'dca') {
    if (typeof i.sliceAmount !== 'bigint' || i.sliceAmount <= 0n) return false;
    fill = i.sliceAmount < i.amount ? i.sliceAmount : i.amount;
  } else return false;
  const price = typeof i.priceEnd === 'bigint' && i.priceEnd > i.price ? i.priceEnd : i.price;
  const value = (fill * price) / scale;
  return value > 0n && value < line;
}

/** The state of the "?" flash: whether the order was dust at the last look, and the flashes so far. */
export interface DustFlash {
  dust: boolean;
  n: number;
}
export const NO_FLASH: DustFlash = { dust: false, n: 0 };

/**
 * The next state: one more flash when the order turns dust, none while it stays dust or is not. It
 * may flash again only after it was not dust in between.
 */
export const nextDustFlash = (s: DustFlash, dust: boolean): DustFlash => (dust === s.dust ? s : { dust, n: dust ? s.n + 1 : s.n });

/**
 * The "?"'s flash class: the confirmation's price flash (`send-flash-0` / `send-flash-1` of
 * ui/confirm/send.css), two names so that the next flash restarts the animation. None before the
 * first.
 */
export const dustFlashClass = (s: DustFlash): string => (s.n > 0 ? ` tk-flash-${s.n % 2}` : '');
