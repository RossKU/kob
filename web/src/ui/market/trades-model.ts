// View models of the recent-trades list. Pure.
//
// A `fill` event names the order that was filled. `side` 1 means an ASK, a sell order, was filled,
// so the taker bought. `side` 2 means a bid was filled, so the taker sold. Prices are the orders'
// state prices, in sompi per whole token of `scale` base units (see book-model.ts), and amounts
// are in base units. `ts` is the block timestamp in ms, where 0 means unknown.
import type { EventView } from '../../data/indexer-types';
import { quoteOf } from '../../kob/units';

export interface TradeRow {
  id: number;
  txid: string;
  covenantId: string;
  /** What the taker did: `buy` when an ask was filled. */
  takerSide: 'buy' | 'sell' | null;
  /** Sompi per whole token (`scale` base units). */
  price: bigint | null;
  /** Base units filled. */
  amount: bigint | null;
  /** The fill's KAS value, `amount x price / scale` rounded down, in sompi. */
  value: bigint | null;
  /** Block time in ms since epoch. Null when the indexer does not know it. */
  timeMs: number | null;
  daa: number;
  settled: boolean;
  confirmations: number | null;
}

const big = (v: unknown): bigint | null => (typeof v === 'string' && /^\d+$/.test(v) ? BigInt(v) : typeof v === 'number' && Number.isSafeInteger(v) && v >= 0 ? BigInt(v) : null);

/**
 * Turns fill events into trade rows, newest first. `scale` is the market's scale, the base units
 * per whole token and the prices' denominator.
 */
export function tradeRows(items: readonly EventView[], scale: bigint, limit = 30): TradeRow[] {
  const sc = scale > 0n ? scale : 1n;
  return items
    .filter((e) => e.kind === 'fill')
    .map((e): TradeRow => {
      const price = big(e.price);
      const amount = big(e.amount);
      return {
        id: e.id,
        txid: e.txid,
        covenantId: e.covenant_id,
        takerSide: e.side === 1 ? 'buy' : e.side === 2 ? 'sell' : null,
        price,
        amount,
        value: price !== null && amount !== null ? quoteOf(amount, price, sc, 'down') : null,
        timeMs: typeof e.ts === 'number' && e.ts > 0 ? e.ts : null,
        daa: e.daa,
        settled: e.settled,
        confirmations: e.confirmations,
      };
    })
    .sort((a, b) => b.daa - a.daa || b.id - a.id)
    .slice(0, limit);
}

/**
 * Amount-weighted average price of the rows that have a price and an amount, in sompi per whole
 * token of `scale` base units, rounded down. It is null when there is nothing to weigh.
 */
export function vwap(rows: readonly TradeRow[]): bigint | null {
  let weighted = 0n;
  let amount = 0n;
  for (const r of rows) {
    if (r.price === null || r.amount === null || r.amount <= 0n) continue;
    weighted += r.price * r.amount;
    amount += r.amount;
  }
  return amount > 0n ? weighted / amount : null;
}
