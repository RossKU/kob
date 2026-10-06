// View models of the recent-trades list. Pure.
//
// A `fill` event names the order that was filled: `side` 1 = an ASK (sell order) was filled, i.e. the TAKER bought; 2 = a bid was filled,
// the taker sold. Prices are the orders' state prices (sompi per whole token of `scale` base units, see book-model.ts), amounts base units.
// `ts` is the block timestamp in ms (0 = unknown).
import type { EventView } from '../../data/indexer-types';
import { quoteOf } from '../../kob/units';

export interface TradeRow {
  id: number;
  txid: string;
  covenantId: string;
  /** what the taker did: `buy` when an ask was filled */
  takerSide: 'buy' | 'sell' | null;
  /** sompi per whole token (`scale` base units) */
  price: bigint | null;
  /** base units filled */
  amount: bigint | null;
  /** the fill's KAS value, `amount x price / scale` rounded down, sompi */
  value: bigint | null;
  /** block time, ms since epoch; null when the indexer does not know it */
  timeMs: number | null;
  daa: number;
  settled: boolean;
  confirmations: number | null;
}

const big = (v: unknown): bigint | null => (typeof v === 'string' && /^\d+$/.test(v) ? BigInt(v) : typeof v === 'number' && Number.isSafeInteger(v) && v >= 0 ? BigInt(v) : null);

/** Fill events -> trade rows, newest first. `scale` is the market's scale (base units per whole token: the prices' denominator). */
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
 * Amount-weighted average price of the rows that have a price and an amount (sompi per whole token of `scale` base units, rounded down); null
 * when there is nothing to weigh.
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
