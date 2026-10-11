// A trader limit that is marketable when it is placed is a taker order, not a resting limit: it becomes an IOC priced at the intended limit
// (the slippage bound), so it fills what crosses up to that price and returns the rest instead of auctioning or resting through the opposite
// side. A passive limit (one that does not cross the opposite best price) is left alone and rests. Pure: no `@/` runtime imports.
import type { Intent } from '@/kob/plan';

/** One price level of a book (a KAS book: sompi per whole token; a pair book: B base units per whole A); `tip` is the level's priority tip. */
export interface LevelLike {
  price: bigint;
  tip?: bigint;
}

/** The sides of a book the check reads: asks and bids, best first. */
export interface BookLike {
  asks: readonly LevelLike[];
  bids: readonly LevelLike[];
}

/**
 * True when a limit on `side` at `price` (with its `tip`) meets the opposite best level: the buyer's all-in ceiling reaches the seller's
 * all-in floor (equality crosses; the planner's `crosses`). A pair book carries no tips on either side (`tip` 0).
 */
export function crossesBest(side: 'buy' | 'sell', price: bigint, tip: bigint, book: BookLike): boolean {
  const lv = side === 'sell' ? book.bids[0] : book.asks[0];
  if (!lv) return false;
  const theirs = lv.tip ?? 0n;
  return side === 'sell' ? lv.price + theirs >= price - tip : price + tip >= lv.price - theirs;
}

/**
 * The intent to place for `intent`: a plain limit (GTC / GTD / day, active now) that crosses the opposite best price of `book` becomes an IOC
 * at the same price and tip (the limit is the slippage bound); every other intent, and every passive limit, is returned unchanged.
 * `pair` = a pair book (B prices carry no KAS tips).
 */
export function takerIntent(intent: Intent, book: BookLike, pair = false): { intent: Intent; taker: boolean } {
  if (intent.type !== 'limit' || intent.activeFrom) return { intent, taker: false };
  if (!crossesBest(intent.side, intent.price, pair ? 0n : intent.tip ?? 0n, book)) return { intent, taker: false };
  return { intent: { type: 'ioc', side: intent.side, amount: intent.amount, price: intent.price, ...(intent.tip !== undefined ? { tip: intent.tip } : {}) }, taker: true };
}
