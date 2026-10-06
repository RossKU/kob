import { describe, expect, it } from 'vitest';
import type { CandleView, DepthView, StatsView, TradesView } from '../../data/indexer-types';
import { buildBookModel } from './book-model';
import {
  basisToNumber, basisToTokenSompi, bpsPercentText, chartSeries, compactText, cumulate, depthFromBook, depthFromView, depthGeometry, fillCandleGaps,
  INTERVAL_MS, niceTicks, parseCandles, priceDecimals, priceText, statsModel, tapeFromFills, tapeFromTrades, tapeTime, amountText, type Candle,
} from './market-model';
import { tradeRows } from './trades-model';

const TOK = '11'.repeat(32);
const BASIS = 100_000_000n; // one whole token of 8 decimals = the standard scale

describe('price basis conversion', () => {
  it('converts sompi per basis to KAS per whole token (contract: price / 1e8 * 10^decimals / basis)', () => {
    // 1234500000 sompi per 1e8 base units (8 decimals) = 12.345 KAS per token
    expect(basisToNumber(1_234_500_000n, 8, BASIS)).toBeCloseTo(12.345, 12);
    expect(basisToTokenSompi(1_234_500_000n, 8, BASIS)).toBe(1_234_500_000n);
    // basis = 100 base units of a 2-decimal token: 250 sompi per 1.00 token -> 0.0000025 KAS
    expect(basisToNumber(250n, 2, 100n)).toBeCloseTo(0.0000025, 15);
    // a price per 1e6 base units of an 8-decimal token (0.01 token, an open-list order scale): 25_000 sompi per 0.01 token = 2_500_000 sompi per token
    expect(basisToTokenSompi(25_000n, 8, 1_000_000n)).toBe(2_500_000n);
    expect(basisToNumber(25_000n, 8, 1_000_000n)).toBeCloseTo(0.025, 12);
    expect(basisToNumber(5n, 8, 0n)).toBe(0);
  });

  it('chooses one fixed number of decimals per market (about 5 significant digits, 2..10)', () => {
    expect(priceDecimals(1_234_500_000n)).toBe(3); // 12.345
    expect(priceDecimals(2_500_000n)).toBe(6); // 0.025000
    expect(priceDecimals(250n)).toBe(10); // 0.0000025 -> capped at 10
    expect(priceDecimals(123_456_789_000_000n)).toBe(2); // 1,234,567.89
    expect(priceDecimals(null)).toBe(4);
  });

  it('formats exact prices with fixed decimals and grouping, amounts trimmed', () => {
    expect(priceText(1_234_500_000n, 8, BASIS, 3)).toBe('12.345');
    expect(priceText(1_234_560_000n, 8, BASIS, 4)).toBe('12.3456');
    expect(priceText(2_500_000n, 8, BASIS, 6)).toBe('0.025000');
    expect(priceText(123_456_789_000_000n, 8, BASIS, 2)).toBe('1,234,567.89');
    // the spread of a crossed book is negative (found live on TN10: the market page crashed in roundDiv)
    expect(priceText(-2_500_000n, 8, BASIS, 6)).toBe('-0.025000');
    expect(priceText(-2_500_000n, 8, 0n, 3)).toBe('-0.025');
    expect(priceText(25_000n, 8, 1_000_000n, 6)).toBe('0.025000');
    expect(amountText(300_000_000n, 8)).toBe('3');
    expect(amountText(123_456_789_012n, 8, 2)).toBe('1,234.57');
    expect(amountText(5n, 0)).toBe('5');
  });

  it('signed percent text of basis points', () => {
    expect(bpsPercentText(287)).toBe('+2.87%');
    expect(bpsPercentText(-40)).toBe('-0.40%');
    expect(bpsPercentText(0)).toBe('0.00%');
    expect(bpsPercentText(-12345)).toBe('-123.45%');
  });

  it('compact figures', () => {
    expect(compactText(1_250_000)).toBe('1.25M');
    expect(compactText(950)).toBe('950');
    expect(compactText(1.23456)).toBe('1.2346');
    expect(compactText(Number.NaN)).toBe('—');
  });
});

describe('candles', () => {
  const cv = (t: number, o: number, h: number, l: number, c: number, volume = 100): CandleView => ({
    t, o: String(o), h: String(h), l: String(l), c: String(c), volume: String(volume), quote_volume: String(volume * c), trades: 1,
  });
  const M = INTERVAL_MS['1m'];

  it('parses, sorts, deduplicates and drops malformed buckets', () => {
    const c = parseCandles([cv(2 * M, 5, 6, 4, 5), cv(0, 1, 2, 1, 2), { ...cv(M, 1, 1, 1, 1), o: '-1' }, cv(2 * M, 7, 8, 6, 7)]);
    expect(c.map((x) => [x.t, x.o])).toEqual([[0, 1n], [2 * M, 7n]]);
    expect(c[0]!.quote).toBe(200n);
  });

  it('carries the previous close across empty buckets (flat, zero volume, marked filled)', () => {
    const c = parseCandles([cv(0, 10, 12, 9, 11), cv(3 * M, 11, 15, 11, 14)]);
    const f = fillCandleGaps(c, M);
    expect(f.map((x) => x.t)).toEqual([0, M, 2 * M, 3 * M]);
    expect(f[1]).toEqual({ t: M, o: 11n, h: 11n, l: 11n, c: 11n, volume: 0n, quote: 0n, trades: 0, filled: true });
    expect(f[2]!.c).toBe(11n);
    expect(f[3]!.filled).toBe(false);
    expect(f[3]!.h).toBe(15n);
  });

  it('extends to the bucket of `until` and keeps only the newest maxBars (carrying the close from before the window)', () => {
    const c = parseCandles([cv(0, 10, 10, 10, 10), cv(M, 10, 20, 10, 20)]);
    const f = fillCandleGaps(c, M, { until: 4 * M + 30_000 });
    expect(f.map((x) => x.t)).toEqual([0, M, 2 * M, 3 * M, 4 * M]);
    expect(f.at(-1)!.c).toBe(20n);
    const w = fillCandleGaps(c, M, { until: 1_000 * M, maxBars: 3 });
    expect(w.map((x) => x.t)).toEqual([998 * M, 999 * M, 1000 * M]);
    expect(w.every((x) => x.filled && x.c === 20n)).toBe(true);
    // a long sparse history does not allocate a bar per empty minute
    const huge = fillCandleGaps(parseCandles([cv(0, 1, 1, 1, 1), cv(5_000_000 * M, 2, 2, 2, 2)]), M, { maxBars: 10 });
    expect(huge).toHaveLength(10);
    expect(huge.at(-1)!.c).toBe(2n);
    expect(fillCandleGaps([], M)).toEqual([]);
  });

  it('converts to chart series: UTC seconds, KAS per token, whole-token volume, up/down', () => {
    const c: Candle[] = [
      { t: 60_000, o: 1_000_000_000n, h: 1_200_000_000n, l: 900_000_000n, c: 1_100_000_000n, volume: 250_000_000n, quote: 0n, trades: 3, filled: false },
      { t: 120_000, o: 1_100_000_000n, h: 1_100_000_000n, l: 1_000_000_000n, c: 1_000_000_000n, volume: 0n, quote: 0n, trades: 0, filled: true },
    ];
    const s = chartSeries(c, 8, BASIS);
    expect(s.bars[0]).toEqual({ time: 60, open: 10, high: 12, low: 9, close: 11, filled: false });
    expect(s.bars[1]!.filled).toBe(true);
    expect(s.volumes).toEqual([{ time: 60, value: 2.5, up: true }, { time: 120, value: 0, up: false }]);
  });
});

describe('24h stats', () => {
  const stats: StatsView = {
    token: TOK, price_basis: '100000000', ts: 1, last: '1234500000', last_ts: 1, last_side: 'buy', open_24h: '1200000000', high_24h: '1250000000',
    low_24h: '1190000000', change_24h_bps: 287, volume_24h: '90000000000', quote_volume_24h: '1110000000000', trades_24h: 412,
    best_bid: '1233000000', best_ask: '1236000000', mid: '1234500000', spread_bps: 24, open_asks: 18, open_bids: 21,
  };
  it('formats last / change / high / low / volumes with one precision', () => {
    const m = statsModel(stats, 8);
    expect(m).toMatchObject({
      last: '12.345', lastSide: 'buy', change: '+2.87%', changeTone: 'up', high: '12.500', low: '11.900', volume: '900', volumeExact: '900',
      quoteVolume: '11.1K', quoteVolumeExact: '11,100', trades: 412, spread: '0.24%', dp: 3,
    });
    expect(statsModel({ ...stats, change_24h_bps: -5 }, 8).changeTone).toBe('down');
    expect(statsModel({ ...stats, change_24h_bps: 0 }, 8).change).toBe('0.00%');
  });
  it('is all dashes without trades or without the endpoint', () => {
    const none = statsModel({ ...stats, last: null, last_side: null, open_24h: null, high_24h: null, low_24h: null, change_24h_bps: null, volume_24h: null, quote_volume_24h: null, trades_24h: null }, 8);
    expect(none).toMatchObject({ last: null, change: null, changeTone: null, high: null, volume: null, trades: null, spread: '0.24%' });
    // the book mid still sets the precision
    expect(none.dp).toBe(3);
    expect(statsModel(null, 8)).toMatchObject({ last: null, change: null, spread: null, dp: 4 });
    expect(statsModel({ ...stats, price_basis: '0' }, 8).last).toBeNull();
  });
});

describe('trade tape', () => {
  it('maps /v1/trades (aggressor side, amounts, basis)', () => {
    const v: TradesView = {
      token: TOK, price_basis: '100000000', next_cursor: null,
      items: [
        { id: 9, txid: TOK, ts: 1_759_160_000_123, daa: 5, price: '1234500000', amount: '300000000', quote: '3703500000', side: 'sell', fills: 3, confirmations: 2, settled: false },
        { id: 7, txid: TOK, ts: 0, daa: 4, price: 'x', amount: '1', quote: '1', side: 'buy', fills: 1, confirmations: 2, settled: true },
      ],
    };
    const t = tapeFromTrades(v);
    expect(t.basis).toBe(BASIS);
    expect(t.source).toBe('trades');
    expect(t.rows[0]).toEqual({ id: '9', txid: '1'.repeat(64), timeMs: 1_759_160_000_123, daa: 5, side: 'sell', price: 1_234_500_000n, amount: 300_000_000n, quote: 3_703_500_000n, settled: false, fills: 3 });
    expect(t.rows[1]).toMatchObject({ timeMs: null, price: null, side: 'buy' });
    expect(tapeFromTrades(v, 1).rows).toHaveLength(1);
  });
  it('falls back to fill events (the orders\' state prices, basis = the market scale)', () => {
    const rows = tradeRows(
      [{ id: 3, covenant_id: TOK, block_seq: 1, daa: 10, ts: 5000, txid: TOK, tx_pos: 0, kind: 'fill', token: TOK, side: 1, amount: '200000000', price: '2500000', payout: null, closes: false, detail: null, confirmations: 1, settled: true }],
      BASIS,
    );
    const t = tapeFromFills(rows, BASIS);
    expect(t).toMatchObject({ basis: BASIS, source: 'fills' });
    expect(t.rows[0]).toMatchObject({ side: 'buy', price: 2_500_000n, amount: 200_000_000n, quote: 5_000_000n, timeMs: 5000 });
    // without a known scale the prices stay per the orders' own basis (the amount is still known)
    expect(tapeFromFills(rows, null)).toMatchObject({ basis: null, rows: [{ amount: 200_000_000n }] });
  });
  it('prints the local time of day', () => {
    const d = new Date(2026, 0, 2, 3, 4, 5);
    expect(tapeTime(d.getTime())).toBe('03:04:05');
    expect(tapeTime(null)).toBe('--:--:--');
  });
});

describe('depth', () => {
  it('cumulates from the best price outwards', () => {
    expect(cumulate([{ price: 5n, amount: 2n }, { price: 4n, amount: 3n }]).map((l) => l.cum)).toEqual([2n, 5n]);
  });

  it('builds the series from /v1/depth, sorting best first and recomputing the sums', () => {
    const v: DepthView = {
      token: TOK, price_basis: '100000000', ts: 1, daa: 2,
      bids: [
        { price: '1200000000', amount: '100000000', orders: 1, cum_amount: '999', cum_quote: '0', estimated: false },
        { price: '1233000000', amount: '500000000', orders: 2, cum_amount: '500000000', cum_quote: '0', estimated: true },
      ],
      asks: [{ price: '1236000000', amount: '300000000', orders: 1, cum_amount: '300000000', cum_quote: '0', estimated: false }],
    };
    const s = depthFromView(v, 8)!;
    expect(s.bids.map((p) => [p.price, p.cum])).toEqual([[12.33, 5], [12, 6]]);
    expect(s.asks.map((p) => [p.price, p.cum])).toEqual([[12.36, 3]]);
    expect(s.mid).toBeCloseTo(12.345, 10);
    expect(s.estimated).toBe(true);
    expect(s.bidTotal).toBe(600_000_000n);
    expect(depthFromView({ ...v, price_basis: '0' }, 8)).toBeNull();
  });

  it('builds the series from the book when the indexer has no depth route', () => {
    // 7 and 4 whole tokens at 0.0251 / 0.0249 KAS per token (prices per 1e8 base units)
    const lv = (price: string, amount: string, orders: number) => ({ price, amount, amount_estimated: false, orders, scale: 100_000_000 });
    const m = buildBookModel({ asks: [lv('2510000', '700000000', 2)], bids: [lv('2490000', '400000000', 1)] }, { scale: BASIS });
    const s = depthFromBook(m, 8, BASIS);
    expect(s.asks).toEqual([{ price: 0.0251, cum: 7 }]);
    expect(s.bids).toEqual([{ price: 0.0249, cum: 4 }]);
    expect(s.mid).toBeCloseTo(0.025, 12);
    expect(s.askTotal).toBe(700_000_000n);
  });

  it('draws both sides around the mid, symmetric, with nice ticks', () => {
    const g = depthGeometry({ bids: [{ price: 9, cum: 1 }, { price: 8, cum: 3 }], asks: [{ price: 11, cum: 2 }], mid: 10, estimated: false, bidTotal: 0n, askTotal: 0n }, 400, 200);
    expect(g.hasData).toBe(true);
    expect(g.midX).toBeCloseTo(200, 6);
    expect(g.xMin).toBeCloseTo(10 - 2 * 1.04, 9);
    expect(g.bidArea.startsWith('M')).toBe(true);
    expect(g.bidArea.endsWith('Z')).toBe(true);
    expect(g.askLine).toContain('L400');
    expect(g.yTicks.every((t) => t.y >= 0 && t.y <= 200)).toBe(true);
    expect(g.xTicks.length).toBeGreaterThan(1);
    expect(depthGeometry({ bids: [], asks: [], mid: null, estimated: false, bidTotal: 0n, askTotal: 0n }, 10, 10).hasData).toBe(false);
  });

  it('nice ticks', () => {
    expect(niceTicks(0, 10, 5)).toEqual([0, 2, 4, 6, 8, 10]);
    expect(niceTicks(0.0241, 0.0259, 4)).toEqual([0.0245, 0.025, 0.0255]);
    expect(niceTicks(1, 1)).toEqual([]);
  });
});
