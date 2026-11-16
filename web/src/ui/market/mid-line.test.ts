import { describe, expect, it } from 'vitest';
import type { CandlesQuery, IndexerApi } from '../../data/indexer';
import type { QuotesView } from '../../data/indexer-types';
import { loadQuotes, mergeQuotes, midOf, midOnBars, midSeries, type QuoteItem } from './mid-line';

const view = (items: QuoteItem[], basis = '1000'): QuotesView => ({ token: 'aa', interval: '1m', price_basis: basis, decimals: 3, items });
const signal = new AbortController().signal;

describe('midOf', () => {
  it('is (bid + ask) / 2 rounded down, and none with an empty or unreadable side', () => {
    expect(midOf([0, '100', '201'])).toBe(150n);
    expect(midOf([0, null, '200'])).toBeNull();
    expect(midOf([0, '100', null])).toBeNull();
    expect(midOf([0, 'x', '200'])).toBeNull();
  });
});

describe('midOnBars', () => {
  it('carries the last quote before each bar ends, none before the first, and a gap where a side emptied', () => {
    const items: QuoteItem[] = [[60_000, '100', '200'], [150_000, null, '200'], [240_000, '120', '180']];
    // bars at 0, 60, 120, 180, 240, 300 s, one minute each
    expect(midOnBars(items, [0, 60, 120, 180, 240, 300], 60_000)).toEqual([null, 150n, null, null, 150n, 150n]);
    expect(midOnBars([], [0, 60], 60_000)).toEqual([null, null]);
  });
});

describe('midSeries', () => {
  it('draws KAS per whole token (or its inverse), a gap for a bar without a mid, and the legend text at the market decimals', () => {
    // decimals 3, basis 1000: a price per basis is sompi per whole token
    const m = midSeries([null, 150_000_000n, null], [0, 60, 120], 3, 1000n, 2, false);
    expect(m.points).toEqual([{ time: 0 }, { time: 60, value: 1.5 }, { time: 120 }]);
    expect([m.hasData, m.text(0), m.text(1)]).toEqual([true, null, '1.50']);
    const inv = midSeries([200_000_000n], [0], 3, 1000n, 2, true);
    expect(inv.points).toEqual([{ time: 0, value: 0.5 }]);
    expect(inv.text(0)).toBe('0.50');
    expect(midSeries([null, null], [0, 60], 3, 1000n, 2, false).hasData).toBe(false);
  });
});

describe('mergeQuotes', () => {
  it('keeps the items before `from`, takes the newer ones from the fresh answer, and keeps the cache when it has none', () => {
    const old: QuoteItem[] = [[1, '1', '2'], [2, '1', '3']];
    expect(mergeQuotes(old, [[2, '1', '4'], [3, '2', '4']], 2, 10)).toEqual([[1, '1', '2'], [2, '1', '4'], [3, '2', '4']]);
    expect(mergeQuotes(old, [], 2, 10)).toEqual(old);
    expect(mergeQuotes(old, [[3, '2', '4']], 3, 2)).toEqual([[2, '1', '3'], [3, '2', '4']]);
  });
});

describe('loadQuotes', () => {
  it('reads the window up to now, then only from its newest bucket; none without the route; a failed read keeps what it had', async () => {
    const calls: CandlesQuery[] = [];
    let fail = false;
    const api = {
      quotes: async (_t: string, q: CandlesQuery) => {
        calls.push(q);
        if (fail) throw new Error('down');
        return view(q.from === 120_000 ? [[120_000, '5', '7']] : [[60_000, '1', '3'], [120_000, '2', '4']]);
      },
    } as unknown as IndexerApi;
    const first = await loadQuotes(api, 'aa', '1m', null, signal, 10, 200_000);
    expect(calls[0]).toMatchObject({ interval: '1m', from: 180_000 - 9 * 60_000, limit: 10 });
    expect(first?.items).toEqual([[60_000, '1', '3'], [120_000, '2', '4']]);
    const next = await loadQuotes(api, 'aa', '1m', first, signal, 10, 200_000);
    expect(calls[1]).toMatchObject({ from: 120_000 });
    expect(next?.items).toEqual([[60_000, '1', '3'], [120_000, '5', '7']]);
    fail = true;
    expect(await loadQuotes(api, 'aa', '1m', next, signal, 10)).toBe(next);
    expect(await loadQuotes({} as IndexerApi, 'aa', '1m', null, signal, 10)).toBeNull();
  });
});
