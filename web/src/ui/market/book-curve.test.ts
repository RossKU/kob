// A curve sell order's levels in the book: one row per remaining level (`curve_level`, the same covenant id), summed with the other orders at
// each price like any order, and marked; grouping keeps the marks.
import { describe, expect, it } from 'vitest';
import type { BookOrderView } from '../../data/indexer-types';
import { buildBookModel, groupLevels, levelsFromView } from './book-model';
import { curveMark } from './OrderBook';

const CURVE = 'cc'.repeat(32);
const row = (price: number, amount: number, over: Partial<BookOrderView> = {}): BookOrderView => ({
  covenant_id: 'aa'.repeat(32), contract: 'KobAsk', maker: null, price: String(price), tip: '0', min_fill: '1', scale: 100, amount_left: String(amount),
  amount_estimated: false, status: 'open', cur_value: '1', expiry_daa: null, expired: false, genesis_daa: 1, confirmations: 1, settled: true, ...over,
});
const curveRow = (level: number, price: number, amount: number) => row(price, amount, { covenant_id: CURVE, contract: 'KobCurveAsk', curve_level: level });

describe('curve levels in the book', () => {
  it('sum with other orders at a price and carry their level', () => {
    const ls = levelsFromView([row(100, 5), curveRow(3, 100, 2), curveRow(4, 110, 2), row(120, 1)], 100n);
    expect(ls.map((l) => [l.price, l.amount, l.orders, l.curve ?? null])).toEqual([
      [100n, 7n, 2, [3]],
      [110n, 2n, 1, [4]],
      [120n, 1n, 1, null],
    ]);
  });

  it('keep their marks through grouping and into the rows', () => {
    const ls = levelsFromView([curveRow(3, 101, 2), curveRow(4, 105, 2), row(109, 1)], 100n);
    const g = groupLevels(ls, 10n, 'ask');
    expect(g.map((l) => [l.price, l.curve ?? null])).toEqual([[110n, [3, 4]]]);
    const m = buildBookModel({ asks: [curveRow(3, 101, 2), row(150, 1)], bids: [] }, { scale: 100n });
    expect(m.asks.map((r) => r.curve ?? null)).toEqual([[3], null]);
  });

  it('title the level as people count (1-based)', () => {
    expect(curveMark([11])).toBe('Level 12 of a curve order');
    expect(curveMark([3, 4])).toBe('Levels 4, 5 of curve orders');
  });
});
