import { describe, expect, it } from 'vitest';
import type { PairBookView, PairCandleView, PairCandlesView, PairFillView, PairFillsView, PairLevelView } from '../../data/indexer-types';
import type { PairToken } from '../../kob/pair';
import { DAY_MS, pairDepthOf, pairDepthSeries, pairRatioCandles, pairStatsModel, pairVolumeOf } from './pair-market';
import type { RatioCandle } from './usd-model';

// BASE: 8 decimals, scale 1e8; QUOTE: 6 decimals, scale 1e6.
const BASE: PairToken = { covenantId: 'aa'.repeat(32), ticker: 'BTC', decimals: 8, scale: 100_000_000n };
const QUOTE: PairToken = { covenantId: 'bb'.repeat(32), ticker: 'USDT', decimals: 6, scale: 1_000_000n };
const BTC = 100_000_000n;

const fillsView = (items: PairFillView[], vol = { amount_a: '0', amount_b: '0', fills: 0 }): PairFillsView => ({ base: BASE.covenantId, quote: QUOTE.covenantId, volume_24h: vol, items, next_cursor: null });

describe('pair fills: the 24 h volume (a pair fill never makes a price)', () => {
  it('the 24 h pair volume of the fills view; none for a view of another pair', () => {
    const v = fillsView([], { amount_a: '300000000', amount_b: '151500', fills: 2 });
    expect(pairVolumeOf(v, BASE, QUOTE)).toEqual({ a: 300_000_000n, b: 151_500n, fills: 2 });
    expect(pairVolumeOf(v, QUOTE, BASE)).toBeNull();
    expect(pairVolumeOf(null, BASE, QUOTE)).toBeNull();
  });
});

describe('pair candles of the indexer (derived from the two KAS series)', () => {
  const rate = (num: string, den = '1') => ({ value: String(BigInt(num) / BigInt(den)), num, den });
  const c = (t: number, o: string, h: string, l: string, cl: string, va = '0', vb = '0', n = 0, traded = true): PairCandleView => ({
    t, o: rate(o), h: rate(h), l: rate(l), c: rate(cl), a_traded: traded, b_traded: false, pair_volume_a: va, pair_volume_b: vb, pair_fills: n,
  });
  const view = (items: PairCandleView[], basis = '100000000'): PairCandlesView => ({
    base: BASE.covenantId, quote: QUOTE.covenantId, interval: '5m', price_basis: basis, quote_price_basis: '1000000', decimals: 8, quote_decimals: 6, price_source: 'kas_books', items,
  });

  it('rates in B base units per price_basis base units of A become whole QUOTE per whole BASE; the volume is the pair volume', () => {
    // 50,500 USDT units per 1e8 BTC units = 0.0505 USDT per BTC
    const [x] = pairRatioCandles(view([c(60_000, '50500', '51000', '50000', '50500', '200000000', '101000', 1)]), BASE, QUOTE)!;
    expect(x!.o).toEqual({ num: 101n, den: 2000n });
    expect(x!.h).toEqual({ num: 51n, den: 1000n });
    expect([x!.volume, x!.quoteVolume, x!.trades, x!.filled]).toEqual([2, 0.101, 1, false]);
    // a basis of 1 base unit: 505 USDT units per BTC unit = 505 * 1e8 / 1e6 USDT per BTC
    const [y] = pairRatioCandles(view([c(0, '505', '505', '505', '505')], '1'), BASE, QUOTE)!;
    expect(y!.c).toEqual({ num: 50_500n, den: 1n });
  });

  it('a bucket where neither token traded in KAS is carried (filled); a view of another pair or without a basis is refused', () => {
    const [x] = pairRatioCandles(view([c(0, '1', '1', '1', '1', '0', '0', 0, false)]), BASE, QUOTE)!;
    expect(x!.filled).toBe(true);
    expect(pairRatioCandles({ ...view([]), base: QUOTE.covenantId }, BASE, QUOTE)).toBeNull();
    expect(pairRatioCandles(view([], '0'), BASE, QUOTE)).toBeNull();
  });
});

const lv = (source: 'direct' | 'entry' | 'route', num: string, den: string, amount: string): PairLevelView => ({ source, price_num: num, price_den: den, amount, orders: 1 });
const book = (asks: PairLevelView[], bids: PairLevelView[]): PairBookView => ({ base: BASE.covenantId, quote: QUOTE.covenantId, daa_score: 1, asks, bids });

describe('pair depth', () => {
  it('cumulates direct, entry and route levels per side in whole BASE, prices in QUOTE per whole BASE, best first', () => {
    // unit price 52_000 / 1e8 = 0.052 USDT per BTC; the route level 0.053
    const s = pairDepthSeries(
      book([lv('route', '53000', '100000000', '100000000'), lv('direct', '52000', '100000000', '300000000')], [lv('route', '48000', '100000000', '500000000'), lv('entry', '49000', '100000000', '100000000')]),
      BASE,
      QUOTE,
    )!;
    expect(s.asks.map((p) => [p.price, p.cum])).toEqual([[0.052, 3], [0.053, 4]]);
    expect(s.bids.map((p) => [p.price, p.cum])).toEqual([[0.049, 1], [0.048, 6]]);
    expect(s.mid).toBeCloseTo(0.0505, 10);
    expect(s.bidTotal).toBe(600_000_000n);
    expect(s.askTotal).toBe(400_000_000n);
  });

  it('a one-sided book has no mid; a view of another pair is refused', () => {
    expect(pairDepthSeries(book([], [lv('direct', '49000', '100000000', '100000000')]), BASE, QUOTE)!.mid).toBeNull();
    expect(pairDepthSeries(book([], []), QUOTE, BASE)).toBeNull();
  });
});

describe('pair 24 h figures: prices from the KAS series, volumes from pair fills', () => {
  const MIN5 = 300_000;
  const REF = 1_800_000_000_000;
  const r = (n: number, d = 1000): { num: bigint; den: bigint } => ({ num: BigInt(n), den: BigInt(d) });
  const candle = (ago: number, o: number, h: number, l: number, cl: number, volume: number, trades: number, filled = false): RatioCandle => ({
    t: REF - ago * MIN5, o: r(o), h: r(h), l: r(l), c: r(cl), volume, quoteVolume: volume * (o / 1000), trades, filled,
  });
  const common = { last: r(52), changeBps: 120, dp: 4, intervalMs: MIN5, refMs: REF, base: BASE, quote: QUOTE, pairCandles: false, volume: null };

  it('high and low are the extremes of the traded buckets of the last 24 h; carried buckets and older ones do not count', () => {
    const candles = [
      candle(400, 500, 900, 100, 500, 1, 1), // older than 24 h
      candle(100, 50, 55, 45, 52, 2, 3),
      candle(50, 52, 53, 40, 51, 1, 1),
      candle(10, 51, 51, 51, 51, 0, 0, true), // carried
    ];
    const s = pairStatsModel({ ...common, candles });
    expect(s.model.high).toBe('0.0550');
    expect(s.model.low).toBe('0.0400');
    expect(s.model.last).toBe('0.0520');
    expect(s.model.change).toBe('+1.20%');
  });

  it('the volumes and the fill count are the indexer\'s 24 h pair volume; the high / low never come from pair fills', () => {
    const s = pairStatsModel({ ...common, candles: [], volume: { a: 300_000_000n, b: 151_500n, fills: 2 } });
    expect(s.source).toBe('fills');
    expect([s.model.volume, s.model.volumeExact, s.model.quoteVolumeExact, s.model.trades]).toEqual(['3', '3', '0.1515', 2]);
    expect([s.model.high, s.model.low]).toEqual([null, null]);
  });

  it('without the fills view: the pair volume of the indexer\'s pair candles; the client-side ratio candles (KAS volume) are never pair volume', () => {
    const candles = [candle(100, 50, 55, 45, 52, 2, 3), candle(50, 52, 53, 40, 51, 1, 1)];
    const pc = pairStatsModel({ ...common, candles, pairCandles: true });
    expect([pc.source, pc.model.volume, pc.model.trades]).toEqual(['candles', '3', 4]);
    const kas = pairStatsModel({ ...common, candles });
    expect([kas.source, kas.model.volume, kas.model.trades]).toEqual(['none', null, null]);
  });

  it('nothing to show without data: every figure stays empty', () => {
    const s = pairStatsModel({ ...common, last: null, changeBps: null, candles: [] });
    expect(s.source).toBe('none');
    expect([s.model.last, s.model.high, s.model.low, s.model.volume, s.model.quoteVolume, s.model.trades]).toEqual([null, null, null, null, null, null]);
    expect(DAY_MS).toBe(86_400_000);
  });
});

describe('pair depth within 2 % of the mid (the market list)', () => {
  it('sums amount x price (QUOTE base units) of the asks up to +2 % and the bids down to -2 % of the mid, every source, both ends inclusive', () => {
    // best ask 51 000, best bid 50 000 (QUOTE base units per 1e8 BASE base units): mid 50 500, band 49 490 .. 51 510
    const b = book(
      [lv('direct', '51000', '100000000', '300000000'), lv('route', '51510', '100000000', '100000000'), lv('direct', '51511', '100000000', '100000000')],
      [lv('entry', '50000', '100000000', '100000000'), lv('direct', '49490', '100000000', '200000000'), lv('direct', '49489', '100000000', '900000000')],
    );
    // 3 x 51 000 + 1 x 51 510 + 1 x 50 000 + 2 x 49 490
    expect(pairDepthOf(b, BASE, QUOTE)).toEqual({ quote: 153_000n + 51_510n + 50_000n + 98_980n, atLeast: false });
  });

  it('one side only: around the best price of that side; an empty book or a view of another pair has none', () => {
    const bids = book([], [lv('direct', '50000', '100000000', '100000000'), lv('direct', '48999', '100000000', '100000000')]);
    expect(pairDepthOf(bids, BASE, QUOTE)).toEqual({ quote: 50_000n, atLeast: false });
    const asks = book([lv('direct', '50000', '100000000', '100000000'), lv('direct', '51001', '100000000', '100000000')], []);
    expect(pairDepthOf(asks, BASE, QUOTE)).toEqual({ quote: 50_000n, atLeast: false });
    expect(pairDepthOf(book([], []), BASE, QUOTE)).toBeNull();
    expect(pairDepthOf(bids, QUOTE, BASE)).toBeNull();
    expect(pairDepthOf(null, BASE, QUOTE)).toBeNull();
  });

  it('is a lower bound when a side came back full and still inside the band', () => {
    const b = book([lv('direct', '51000', '100000000', '100000000')], [lv('direct', '50000', '100000000', '100000000'), lv('direct', '49900', '100000000', '100000000')]);
    expect(pairDepthOf(b, BASE, QUOTE, 2)).toEqual({ quote: 51_000n + 50_000n + 49_900n, atLeast: true });
    // the bids side is full but the last level it returned is outside the band: nothing beyond it can count
    const c = book([lv('direct', '51000', '100000000', '100000000')], [lv('direct', '50000', '100000000', '100000000'), lv('direct', '40000', '100000000', '100000000')]);
    expect(pairDepthOf(c, BASE, QUOTE, 2)).toEqual({ quote: 101_000n, atLeast: false });
  });
});
