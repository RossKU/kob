import { describe, expect, it } from 'vitest';
import type { CandlesQuery, IndexerApi } from '../../data/indexer';
import type { CandleView, PairCandleView, PairCandlesView } from '../../data/indexer-types';
import { incrementalFrom, loadCandles, mergeBars, mergePairCandleOrientations, type ChartView } from './candle-feed';

const bar = (t: number, c = '100'): CandleView => ({ t, o: c, h: c, l: c, c, volume: '1', quote_volume: '1', trades: 1 });
const kas = (items: CandleView[], basis = '1000') => ({ token: 'aa', interval: '1m' as const, price_basis: basis, decimals: 3, items });
const signal = new AbortController().signal;

/** an indexer whose candles come from `series(token)`, honouring `from` and `limit` like the executor; records every query */
const fakeIndexer = (series: (token: string) => CandleView[], basis = () => '1000') => {
  const calls: { token: string; q: CandlesQuery }[] = [];
  const api = {
    candles: async (token: string, q: CandlesQuery) => {
      calls.push({ token, q });
      const all = series(token).filter((x) => q.from === undefined || x.t >= q.from);
      return kas(all.slice(-(q.limit ?? 500)), basis());
    },
    pairCandles: async () => null,
  } as unknown as IndexerApi;
  return { api, calls };
};

describe('mergeBars', () => {
  it('keeps the bars before `from`, takes every newer bar from the fresh answer (the last bar may have grown), the newest `max`', () => {
    const old = [bar(1), bar(2), bar(3, '100')];
    expect(mergeBars(old, [bar(3, '120'), bar(4)], 3).map((x) => [x.t, x.c])).toEqual([[1, '100'], [2, '100'], [3, '120'], [4, '100']]);
    expect(mergeBars(old, [bar(3), bar(4), bar(5)], 3, 3).map((x) => x.t)).toEqual([3, 4, 5]);
    // the newest bar is gone (a reorg undid its trades): dropped
    expect(mergeBars(old, [], 3).map((x) => x.t)).toEqual([1, 2]);
  });
});

describe('incrementalFrom', () => {
  it('is the newest bar on screen; for two legs the older of their newest bars; none without bars', () => {
    expect(incrementalFrom({ kind: 'kas', v: kas([bar(1), bar(5)]) })).toBe(5);
    expect(incrementalFrom({ kind: 'kas', v: kas([]) })).toBeNull();
    expect(incrementalFrom({ kind: 'usd', q: kas([bar(7)]), b: kas([bar(1), bar(4)]) })).toBe(4);
    expect(incrementalFrom({ kind: 'usd', q: kas([]), b: kas([bar(4)]) })).toBeNull();
  });
});

describe('loadCandles', () => {
  it('the first view loads the full series; a live refresh reads only the bars since the newest one and merges them', async () => {
    const chain = Array.from({ length: 1500 }, (_, i) => bar(i * 60_000));
    const { api, calls } = fakeIndexer(() => chain);
    const first = await loadCandles(api, { token: 'aa' }, '1m', null, signal);
    expect(calls).toEqual([{ token: 'aa', q: { interval: '1m', limit: 1500 } }]);
    expect(first?.kind === 'kas' && first.v.items).toHaveLength(1500);
    // a trade lands in the last bucket and two new buckets open
    chain[1499] = bar(1499 * 60_000, '130');
    chain.push(bar(1500 * 60_000), bar(1501 * 60_000, '90'));
    const next = await loadCandles(api, { token: 'aa' }, '1m', first, signal);
    expect(calls[1]).toEqual({ token: 'aa', q: { interval: '1m', from: 1499 * 60_000, limit: 1500 } });
    if (next?.kind !== 'kas') throw new Error('kas');
    expect(next.v.items).toHaveLength(1500); // still the newest 1500
    expect(next.v.items.at(-3)).toMatchObject({ t: 1499 * 60_000, c: '130' });
    expect(next.v.items.at(-1)).toMatchObject({ t: 1501 * 60_000, c: '90' });
    expect(next.v.items[0]!.t).toBe(2 * 60_000);
  });

  it('falls back to the full load when the newer bars cannot be merged safely: another price basis, an answer as long as the limit, no bars yet', async () => {
    let basis = '1000';
    const chain = [bar(0), bar(60_000)];
    const { api, calls } = fakeIndexer(() => chain, () => basis);
    const first = await loadCandles(api, { token: 'aa' }, '1m', null, signal, 10);
    basis = '10';
    await loadCandles(api, { token: 'aa' }, '1m', first, signal, 10);
    expect(calls.map((c) => c.q.from)).toEqual([undefined, 60_000, undefined]);
    basis = '1000';
    for (let i = 2; i < 20; i++) chain.push(bar(i * 60_000));
    calls.length = 0;
    await loadCandles(api, { token: 'aa' }, '1m', first, signal, 10); // 19 newer buckets, limit 10: some may be missing
    expect(calls.map((c) => c.q.from)).toEqual([60_000, undefined]);
    calls.length = 0;
    await loadCandles(api, { token: 'aa' }, '1m', { kind: 'kas', v: kas([]) }, signal, 10);
    expect(calls.map((c) => c.q.from)).toEqual([undefined]);
  });

  it('a client-side pair ratio refreshes both legs from the older of their newest bars', async () => {
    const legs: Record<string, CandleView[]> = { base: [bar(0), bar(60_000)], quote: [bar(0), bar(120_000)] };
    const { api, calls } = fakeIndexer((t) => legs[t]!);
    const first = (await loadCandles(api, { base: 'base', quote: 'quote' }, '1m', null, signal)) as ChartView;
    expect(first.kind).toBe('usd');
    legs.base!.push(bar(180_000));
    calls.length = 0;
    const next = await loadCandles(api, { base: 'base', quote: 'quote' }, '1m', first, signal);
    expect(calls.map((c) => [c.token, c.q.from])).toEqual([['quote', 60_000], ['base', 60_000]]);
    if (next?.kind !== 'usd') throw new Error('usd');
    expect(next.b.items.map((x) => x.t)).toEqual([0, 60_000, 180_000]);
    expect(next.q.items.map((x) => x.t)).toEqual([0, 120_000]);
  });

  it('the indexer pair candles refresh from their newest bar too; an indexer without them falls back to the two KAS legs', async () => {
    const rate = (v: string) => ({ value: v, num: v, den: '1' });
    const pbar = (t: number, v = '5') => ({ t, o: rate(v), h: rate(v), l: rate(v), c: rate(v), a_traded: true, b_traded: true, pair_volume_a: '0', pair_volume_b: '0', pair_fills: 0 });
    const items = [pbar(0), pbar(60_000)];
    const queries: CandlesQuery[] = [];
    const api = {
      pairCandles: async (_a: string, _b: string, q: CandlesQuery) => {
        queries.push(q);
        return { base: 'a', quote: 'b', interval: '1m', price_basis: '1000', quote_price_basis: '1000', decimals: 3, quote_decimals: 3, price_source: 'kas_books', items: items.filter((x) => q.from === undefined || x.t >= q.from) };
      },
      candles: async () => kas([bar(0)]),
    } as unknown as IndexerApi;
    const first = await loadCandles(api, { base: 'a', quote: 'b', pairCandles: true }, '1m', null, signal);
    items[1] = pbar(60_000, '6');
    items.push(pbar(120_000, '7'));
    const next = await loadCandles(api, { base: 'a', quote: 'b', pairCandles: true }, '1m', first, signal);
    // each load reads both orientations of the pair (a pair fill of QUOTE/BASE is volume of this pair too)
    expect(queries.map((q) => q.from)).toEqual([undefined, undefined, 60_000, 60_000]);
    if (next?.kind !== 'pair') throw new Error('pair');
    expect(next.pc.items.map((x) => [x.t, x.c.value])).toEqual([[0, '5'], [60_000, '6'], [120_000, '7']]);
    const none = { pairCandles: async () => null, candles: async () => kas([bar(0)]) } as unknown as IndexerApi;
    expect((await loadCandles(none, { base: 'a', quote: 'b', pairCandles: true }, '1m', null, signal))?.kind).toBe('usd');
  });
});

describe('pair candles of both orientations', () => {
  const A = 'aa'.repeat(32);
  const B = 'bb'.repeat(32);
  const rate = { value: '2', num: '2', den: '1' };
  const pc = (t: number, a: string, b: string, fills: number): PairCandleView => ({ t, o: rate, h: rate, l: rate, c: rate, a_traded: true, b_traded: true, pair_volume_a: a, pair_volume_b: b, pair_fills: fills });
  const view = (base: string, quote: string, items: PairCandleView[]): PairCandlesView => ({ base, quote, interval: '1m', price_basis: '1000', quote_price_basis: '1000', decimals: 3, quote_decimals: 3, price_source: 'kas_books', items });

  it("adds the other orientation's pair volume to each bucket, its two amounts turned round", () => {
    const ab = view(A, B, [pc(0, '100', '200', 1), pc(60_000, '0', '0', 0)]);
    const ba = view(B, A, [pc(0, '30', '10', 2), pc(60_000, '7', '5', 1), pc(120_000, '9', '9', 9)]);
    const m = mergePairCandleOrientations(ab, ba);
    // volume of A = A's own + what the other view calls B; volume of B = B's own + what it calls A; the other view's extra buckets add nothing
    expect(m.items.map((c) => [c.t, c.pair_volume_a, c.pair_volume_b, c.pair_fills])).toEqual([[0, '110', '230', 3], [60_000, '5', '7', 1]]);
    expect(m.items[0]!.o).toBe(rate);
  });

  it("leaves the view as it is without the other orientation, or with another pair's", () => {
    const ab = view(A, B, [pc(0, '1', '2', 1)]);
    expect(mergePairCandleOrientations(ab, null)).toBe(ab);
    expect(mergePairCandleOrientations(ab, view(A, B, [pc(0, '5', '5', 5)]))).toBe(ab);
  });

  it('the chart load reads both orientations, in full and for the bars since the newest one, and still draws without the other', async () => {
    const calls: { base: string; quote: string; from?: number }[] = [];
    const api = {
      pairCandles: async (base: string, quote: string, q: { from?: number }) => {
        calls.push({ base, quote, ...(q.from !== undefined ? { from: q.from } : {}) });
        if (base === A) return view(A, B, [pc(0, '100', '200', 1)]);
        return view(B, A, [pc(0, '30', '10', 2)]);
      },
    } as unknown as IndexerApi;
    const first = await loadCandles(api, { base: A, quote: B, pairCandles: true }, '1m', null, signal);
    expect(first?.kind === 'pair' && first.pc.items[0]!.pair_volume_a).toBe('110');
    expect(calls.map((c) => c.base === A)).toEqual([true, false]);
    const next = await loadCandles(api, { base: A, quote: B, pairCandles: true }, '1m', first, signal);
    expect(next?.kind === 'pair' && [next.pc.items[0]!.pair_volume_a, next.pc.items[0]!.pair_fills]).toEqual(['110', 3]);
    expect(calls.slice(2).every((c) => c.from === 0)).toBe(true);
    // the other orientation fails: this one alone
    const lone = {
      pairCandles: async (base: string) => {
        if (base !== A) throw new Error('boom');
        return view(A, B, [pc(0, '100', '200', 1)]);
      },
    } as unknown as IndexerApi;
    const v = await loadCandles(lone, { base: A, quote: B, pairCandles: true }, '1m', null, signal);
    expect(v?.kind === 'pair' && v.pc.items[0]!.pair_volume_a).toBe('100');
  });
});
