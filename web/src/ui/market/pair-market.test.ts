import { describe, expect, it } from 'vitest';
import type { PairBookView, PairCandleView, PairCandlesView, PairFillView, PairFillsView, PairLevelView } from '../../data/indexer-types';
import type { PairToken } from '../../kob/pair';
import { DAY_MS, pairDepthSeries, pairFillsOf, pairRatioCandles, pairStatsModel, pairTapeColumns, pairVolumeOf } from './pair-market';
import type { RatioCandle } from './usd-model';

// BASE: 8 decimals, scale 1e8; QUOTE: 6 decimals, scale 1e6.
const BASE: PairToken = { covenantId: 'aa'.repeat(32), ticker: 'BTC', decimals: 8, scale: 100_000_000n };
const QUOTE: PairToken = { covenantId: 'bb'.repeat(32), ticker: 'USDT', decimals: 6, scale: 1_000_000n };
const BTC = 100_000_000n;

let nextId = 1;
const fill = (side: 'ask' | 'bid', a: bigint, b: bigint | null, o: Partial<PairFillView> = {}): PairFillView => ({
  id: nextId++, txid: 'ee'.repeat(32), ts: 1_000_000 + nextId * 1000, daa: 1000 + nextId, order: 'dd'.repeat(32), contract: 'KobPair', side,
  amount_a: a.toString(), amount_b: b === null ? null : b.toString(), price: '5050000', price_num: '101', price_den: '2000', a_scale: 100_000_000,
  tip_kas: '0', counterparty: 'route', price_source: 'none', confirmations: 10, settled: true, ...o,
});
const fillsView = (items: PairFillView[], vol = { amount_a: '0', amount_b: '0', fills: 0 }): PairFillsView => ({ base: BASE.covenantId, quote: QUOTE.covenantId, volume_24h: vol, items, next_cursor: null });

describe('pair fills: volume only (a pair fill never makes a price)', () => {
  it('an ask fill (the order sold BASE): the taker bought; a bid fill: the taker sold; amounts of both tokens and the counterparty, newest first', () => {
    const rows = pairFillsOf(fillsView([fill('ask', 2n * BTC, 101_000n), fill('bid', BTC / 2n, 25_000n, { counterparty: 'netting' })]), BASE, QUOTE);
    expect(rows.map((r) => [r.side, r.baseUnits, r.quoteUnits, r.counterparty])).toEqual([
      ['sell', 50_000_000n, 25_000n, 'netting'],
      ['buy', 200_000_000n, 101_000n, 'route'],
    ]);
    expect(Object.keys(rows[0]!)).not.toContain('price');
  });

  it('drops a view of another pair, malformed amounts and duplicates; a missing B amount stays unknown', () => {
    const dup = fill('ask', BTC, 50_000n);
    const rows = pairFillsOf(fillsView([dup, dup, fill('ask', 0n, 1n), { ...fill('ask', BTC, null), amount_a: 'x' }, fill('bid', BTC, null, { counterparty: 'inventory' })]), BASE, QUOTE);
    expect(rows).toHaveLength(2);
    expect(rows.map((r) => r.quoteUnits)).toEqual([null, 50_000n]);
    expect(pairFillsOf(fillsView([fill('ask', BTC, 1n)]), QUOTE, BASE)).toEqual([]);
    expect(pairFillsOf(null, BASE, QUOTE)).toEqual([]);
  });

  it('columns have one number of decimals per token (the finest amount shown); an unknown B amount shows a dash', () => {
    const rows = pairFillsOf(fillsView([fill('ask', 2n * BTC, 101_000n), fill('bid', 60_000_001n, 3_000_000n), fill('ask', BTC, null)]), BASE, QUOTE);
    const cols = pairTapeColumns(rows, BASE, QUOTE);
    expect(cols.map((c) => c.base)).toEqual(['1.00000000', '0.60000001', '2.00000000']);
    expect(cols.map((c) => c.quote)).toEqual(['—', '3.000', '0.101']);
  });

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
