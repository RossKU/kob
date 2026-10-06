// Book rows that are not liquidity. A bid holds KAS: once fills (or a fee reserve) leave less than one base unit of buying power in its escrow
// the indexer still lists it, with amount 0 (`amount_estimated`), until its maker refunds it. Such rows are dropped here, in the data layer, so no
// consumer (book, depth, best bid, spread, the planners' book) ever sees a "~0" level. The order itself stays in "My orders" (`/v1/orders`, not
// filtered).
import type { BookOrderView, BookView, DepthView, LevelView } from './indexer-types';

const isOrder = (r: LevelView | BookOrderView): r is BookOrderView => 'covenant_id' in r;

/** A row that provably carries nothing: an aggregated level of amount 0, or an order with 0 base units left. An unknown amount is NOT zero. */
export function isEmptyRow(r: LevelView | BookOrderView): boolean {
  const a = isOrder(r) ? r.amount_left : r.amount;
  return a !== null && a !== undefined && /^0+$/.test(String(a));
}

const keepFunded = <T extends LevelView | BookOrderView>(rows: readonly T[]): T[] => rows.filter((r) => !isEmptyRow(r));

/** The book without empty rows (both sides). Returns the same object when nothing is dropped. */
export function dropEmptyRows(v: BookView): BookView {
  if (!Array.isArray(v?.asks) || !Array.isArray(v?.bids)) return v;
  const asks = keepFunded(v.asks);
  const bids = keepFunded(v.bids);
  return asks.length === v.asks.length && bids.length === v.bids.length ? v : { ...v, asks, bids };
}

/** The depth view without levels of amount 0 (cumulative sums are recomputed by the consumer). */
export function dropZeroDepthLevels(v: DepthView): DepthView {
  if (!Array.isArray(v?.asks) || !Array.isArray(v?.bids)) return v;
  const keep = (ls: DepthView['bids']) => ls.filter((l) => !/^0+$/.test(String(l.amount)));
  const bids = keep(v.bids);
  const asks = keep(v.asks);
  return bids.length === v.bids.length && asks.length === v.asks.length ? v : { ...v, bids, asks };
}
