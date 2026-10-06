import { describe, expect, it } from 'vitest';
import type { BookOrderView, LevelView } from '../../data/indexer-types';
import { buildBookModel, depthChartGeometry, groupLevels, groupPrice, groupSizes, inferTick, levelsFromView } from './book-model';

const lv = (price: string, amount: number, orders = 1, est = false, scale = 1): LevelView => ({ price, amount: String(amount), orders, amount_estimated: est, scale });
const ord = (price: string, left: number | null, est = false, scale: number | null = 1): BookOrderView => ({
  covenant_id: 'aa'.repeat(32), contract: 'KobAsk', maker: null, price, tip: null, min_fill: '1', scale, amount_left: left === null ? null : String(left),
  amount_estimated: est, status: 'open', cur_value: null, expiry_daa: null, expired: false, genesis_daa: 1, confirmations: 1, settled: true,
});

describe('bids that cannot fund one base unit', () => {
  it('a level of amount 0 is not a row: not in the depth, not the best bid, not in the spread', () => {
    const m = buildBookModel({ asks: [lv('120', 3)], bids: [lv('118', 0, 1, true), lv('110', 4, 1, true), lv('100', 2)] });
    expect(m.bids.map((r) => r.price)).toEqual([110n, 100n]);
    expect(m.bestBid).toBe(110n);
    expect(m.spread).toBe(10n);
    expect(m.totalBidAmount).toBe(6n);
  });

  it('a per-order row with 0 or unknown amount left is not a row either; a book of only such bids has no bids', () => {
    expect(levelsFromView([ord('118', 0, true), ord('117', null, true), ord('110', 2)], 1n).map((l) => l.price)).toEqual([110n]);
    const m = buildBookModel({ asks: [lv('120', 3)], bids: [lv('118', 0, 1, true)] });
    expect(m.bids).toEqual([]);
    expect(m.bestBid).toBeNull();
    expect(m.spread).toBeNull();
  });

  it('a zero-amount bid above the best ask does not make the spread negative', () => {
    const m = buildBookModel({ asks: [lv('120', 3)], bids: [lv('125', 0, 1, true), lv('110', 1)] });
    expect(m.spread).toBe(10n);
  });
});

describe('levelsFromView', () => {
  it('keeps a price quoted on the market scale and converts a row of another scale to it', () => {
    // 8 decimals: the market scale is 1e8 (one whole token)
    const [l] = levelsFromView([lv('2510000', 7, 2, false, 100_000_000)], 100_000_000n);
    expect(l).toMatchObject({ price: 2_510_000n, amount: 7n, orders: 2, estimated: false });
    // a row quoted per 1000 base units: 25 sompi per 1000 units = 2 500 000 sompi per 1e8 units
    const [o] = levelsFromView([lv('25', 4_000, 1, false, 1000)], 100_000_000n);
    expect(o).toMatchObject({ price: 2_500_000n, amount: 4_000n });
  });

  it('a level or order row carries its own scale and merges on the price per market scale', () => {
    const rows = [lv('5000', 2, 1, false, 1000), lv('50', 3, 2, true, 10)];
    expect(levelsFromView(rows, 1000n)).toEqual([{ price: 5000n, amount: 5n, orders: 3, estimated: true }]);
  });

  it('rounds a converted price to the nearest sompi, and a row without a scale uses the market scale', () => {
    expect(levelsFromView([lv('10', 1, 1, false, 3)], 1n).map((l) => l.price)).toEqual([3n]);
    expect(levelsFromView([lv('11', 1, 1, false, 3)], 1n).map((l) => l.price)).toEqual([4n]);
    expect(levelsFromView([ord('700', 2, false, null)], 100n).map((l) => l.price)).toEqual([700n]);
  });

  it('groups per-order entries by price and keeps the estimate flag sticky', () => {
    const ls = levelsFromView([ord('100', 3), ord('100', null), ord('200', 1, true), ord('100', 2)], 1n);
    const at100 = ls.find((l) => l.price === 100n)!;
    expect(at100).toMatchObject({ amount: 5n, orders: 3, estimated: true }); // an unknown amount left makes the level an estimate
    expect(ls.find((l) => l.price === 200n)!.estimated).toBe(true);
  });

  it('drops malformed rows instead of guessing', () => {
    expect(levelsFromView([lv('abc', 1), lv('0', 1), lv('-5', 1), lv('12', 1)], 1n).map((l) => l.price)).toEqual([12n]);
  });

  it('is exact beyond 2^53', () => {
    // a row of scale 1 on a market of scale 3: the price per 3 base units is 3x
    const [l] = levelsFromView([lv('9007199254740993', 1)], 3n);
    expect(l.price).toBe(27_021_597_764_222_979n);
  });
});

describe('buildBookModel', () => {
  const view = {
    asks: [lv('25300', 8), lv('25100', 7, 2), lv('25200', 10)], // unsorted on purpose
    bids: [lv('24700', 9), lv('24900', 9, 2, true), lv('24800', 11)],
  };

  it('sorts both sides best-first and accumulates depth from the touch', () => {
    const m = buildBookModel(view);
    expect(m.asks.map((r) => r.price)).toEqual([25100n, 25200n, 25300n]);
    expect(m.bids.map((r) => r.price)).toEqual([24900n, 24800n, 24700n]);
    expect(m.asks.map((r) => r.cumAmount)).toEqual([7n, 17n, 25n]);
    expect(m.bids.map((r) => r.cumAmount)).toEqual([9n, 20n, 29n]);
    expect(m.asks[1].cumValue).toBe(25100n * 7n + 25200n * 10n);
    expect(m.asksDisplay.map((r) => r.price)).toEqual([25300n, 25200n, 25100n]); // best ask nearest the spread
  });

  it('values the depth exactly per level: amount x price / scale, rounded down per level', () => {
    // scale 1e8: 1.5 tokens at 2 KAS = 3 KAS; 1 base unit at 1 sompi per token = 0 (rounded down)
    const m = buildBookModel({ asks: [lv('200000000', 150_000_000, 1, false, 1e8), lv('300000000', 1, 1, false, 1e8)], bids: [] }, { scale: 100_000_000n });
    expect(m.asks.map((r) => r.cumValue)).toEqual([300_000_000n, 300_000_003n]);
    const tiny = buildBookModel({ asks: [lv('1', 1, 1, false, 1e8)], bids: [] }, { scale: 100_000_000n });
    expect(tiny.asks[0].cumValue).toBe(0n);
  });

  it('computes spread, mid and bar widths against the deeper side', () => {
    const m = buildBookModel(view);
    expect(m.bestAsk).toBe(25100n);
    expect(m.bestBid).toBe(24900n);
    expect(m.spread).toBe(200n);
    expect(m.mid).toBe(25000n);
    expect(m.spreadBps).toBe(80); // 200 / 25000
    expect(m.totalBidAmount).toBe(29n);
    expect(m.bids[2].depthPct).toBe(100); // the deepest cumulative level fills the bar
    expect(m.asks[2].depthPct).toBe(86.2); // 25 / 29
    expect(m.anyEstimated).toBe(true);
  });

  it('a crossed view has a negative spread (best ask - best bid) and a negative percent', () => {
    const m = buildBookModel({ asks: [lv('100', 1)], bids: [lv('104', 1)] });
    expect(m.spread).toBe(-4n);
    expect(m.mid).toBe(102n);
    expect(m.spreadBps).toBe(-392); // -4 / 102, truncated
    expect(buildBookModel({ asks: [lv('100', 1)], bids: [lv('100', 1)] }).spread).toBe(0n);
  });

  it('handles one-sided and empty books', () => {
    const one = buildBookModel({ asks: [lv('100', 2)], bids: [] });
    expect(one).toMatchObject({ bestBid: null, spread: null, mid: null, spreadBps: null, empty: false });
    const none = buildBookModel({ asks: [], bids: [] });
    expect(none.empty).toBe(true);
    expect(none.asksDisplay).toEqual([]);
  });

  it('limits the depth per side', () => {
    const asks = Array.from({ length: 30 }, (_, i) => lv(String(1000 + i), 1));
    const m = buildBookModel({ asks, bids: [] }, { depth: 5 });
    expect(m.asks).toHaveLength(5);
    expect(m.asks.at(-1)!.price).toBe(1004n);
  });
});

describe('depthChartGeometry', () => {
  it('has no geometry for an empty book', () => {
    expect(depthChartGeometry(buildBookModel({ asks: [], bids: [] })).hasData).toBe(false);
  });

  it('draws closed step areas inside the viewport, bids left of asks', () => {
    const m = buildBookModel({ asks: [lv('110', 5), lv('120', 5)], bids: [lv('90', 4), lv('80', 6)] });
    const g = depthChartGeometry(m, 300, 100);
    expect(g.hasData).toBe(true);
    expect(g.priceMin).toBe(80n);
    expect(g.priceMax).toBe(120n);
    expect(g.bidPath.startsWith('M')).toBe(true);
    expect(g.bidPath.endsWith('Z')).toBe(true);
    expect(g.askPath.endsWith('Z')).toBe(true);
    const nums = (g.bidPath + ' ' + g.askPath).match(/-?\d+(\.\d+)?/g)!.map(Number);
    expect(Math.min(...nums)).toBeGreaterThanOrEqual(0);
    expect(Math.max(...nums)).toBeLessThanOrEqual(300);
    expect(g.midX).toBeCloseTo(((100 - 80) / 40) * 300, 5);
    // the first bid point sits at the best bid x, on the baseline
    expect(g.bidPath.startsWith(`M${(((90 - 80) / 40) * 300).toString()} 100`)).toBe(true);
  });

  it('survives a single price level', () => {
    const g = depthChartGeometry(buildBookModel({ asks: [lv('100', 1)], bids: [] }));
    expect(g.hasData).toBe(true);
    expect(g.priceMin! < g.priceMax!).toBe(true);
    expect(g.askPath).not.toContain('NaN');
  });
});

describe('price aggregation (tick grouping)', () => {
  it('rounds asks up and bids down to the group; exact multiples stay', () => {
    expect(groupPrice(2_510_000n, 100_000n, 'ask')).toBe(2_600_000n);
    expect(groupPrice(2_510_000n, 100_000n, 'bid')).toBe(2_500_000n);
    expect(groupPrice(2_500_000n, 100_000n, 'ask')).toBe(2_500_000n);
    expect(groupPrice(123n, 1n, 'ask')).toBe(123n);
    expect(groupPrice(123n, 0n, 'bid')).toBe(123n);
  });

  it('merges levels on the grouped price (amounts and orders add, estimates stay marked)', () => {
    const g = groupLevels(
      [
        { price: 101n, amount: 2n, orders: 1, estimated: false },
        { price: 109n, amount: 3n, orders: 2, estimated: true },
        { price: 111n, amount: 1n, orders: 1, estimated: false },
      ],
      10n,
      'ask',
    );
    expect(g).toEqual([
      { price: 110n, amount: 5n, orders: 3, estimated: true },
      { price: 120n, amount: 1n, orders: 1, estimated: false },
    ]);
  });

  it('builds a grouped book: cumulative depth and the touch follow the grouping', () => {
    const view = { asks: [lv('101', 2), lv('105', 3), lv('112', 4)], bids: [lv('99', 1), lv('95', 2), lv('88', 5)] };
    const flat = buildBookModel(view);
    expect(flat.asks.map((r) => r.price)).toEqual([101n, 105n, 112n]);
    const m = buildBookModel(view, { group: 10n });
    expect(m.asks.map((r) => [r.price, r.amount, r.cumAmount])).toEqual([[110n, 5n, 5n], [120n, 4n, 9n]]);
    expect(m.bids.map((r) => [r.price, r.amount, r.cumAmount])).toEqual([[90n, 3n, 3n], [80n, 5n, 8n]]);
    expect(m.bestAsk).toBe(110n);
    expect(m.bestBid).toBe(90n);
    expect(m.asks.at(-1)!.depthPct).toBe(100);
  });

  it('infers the tick from the visible prices and offers 1x..1000x', () => {
    expect(inferTick([2_510_000n, 2_520_000n, 2_490_000n])).toBe(10_000n);
    expect(inferTick([])).toBe(1n);
    expect(groupSizes(100n)).toEqual([100n, 1000n, 10_000n, 100_000n]);
    expect(groupSizes(0n)).toEqual([1n, 10n, 100n, 1000n]);
  });
});
