import { describe, expect, it } from 'vitest';
import type { PairSummaryView, StatsView } from '../../data/indexer-types';
import type { TokenInfo } from '../../kob/registry';
import {
  ariaSort, changeCell, DEFAULT_SORT, filterPairs, firstDir, listedPairs, LOADING, nextSort, NONE, pairEntry, pairLabelOf, priceCell, runPool, sortEntries,
  tokenEntry, volumeCell, type ListEntry, type Slot,
} from './market-list';
import type { TokenRow } from './token-model';

const ID = (c: string) => c.repeat(64);

/** A listed registry token row (8 decimals, tick 0.0001 KAS unless given). */
function row(ticker: string, id: string, over: Partial<TokenRow> = {}): TokenRow {
  return {
    covenantId: id, ticker, name: `${ticker} coin`, source: 'registry', info: { covenantId: id, ticker } as unknown as TokenInfo, index: null, delisted: false, powers: [],
    issuerControl: false, templateId: 'kcc20-ref-8x8', badges: [], tradable: true, reason: null, indexerProblems: [], openAsks: 3, openBids: 4, decimals: 8,
    scale: 100_000_000n, tick: 10_000n, ...over,
  };
}

/** `/v1/stats` of a token (prices sompi per whole token of 8 decimals). */
function stats(token: string, over: Partial<StatsView> = {}): StatsView {
  return {
    token, price_basis: '100000000', decimals: 8, ts: 1_700_000_000_000, last: '2510000', last_ts: 1, last_side: 'buy', open_24h: '2500000', high_24h: '2600000',
    low_24h: '2400000', change_24h_bps: 40, volume_24h: '1500000000', quote_volume_24h: '37650000', trades_24h: 7, best_bid: '2500000', best_ask: '2520000',
    mid: '2510000', spread_bps: 79, open_asks: 3, open_bids: 4, ...over,
  };
}
const ok = <T,>(value: T): Slot<T> => ({ state: 'ok', value });

describe('market list cells', () => {
  it('a token row: last price in KAS at the tick decimals with the exact price and TOKEN / KAS unit in the tooltip, change, KAS volume, spread, orders', () => {
    const r = row('EXKCC', ID('a'));
    const e = tokenEntry(r, ok(stats(r.covenantId)));
    expect(e.cells.last).toEqual({ text: '0.0251', title: '0.0251 (EXKCC / KAS)' });
    expect(e.cells.change).toEqual({ text: '+0.40%', title: null, tone: 'up' });
    expect(e.cells.volume).toEqual({ text: '0.38', title: '0.3765 KAS · 7 trades in 24 h' });
    expect(e.cells.spread).toEqual({ text: '0.79%', title: 'Best bid 0.0250 · best ask 0.0252 (EXKCC / KAS)' });
    expect(e.cells.orders).toEqual({ text: '7', title: '3 asks · 4 bids' });
    expect(e.loading).toBe(false);
    expect(e.unavailable).toBe(false);
    expect(e.sort.volume).toBeCloseTo(0.3765);
    expect(e.sort.change).toBe(40);
    expect(e.sort.last).toEqual({ num: 251n, den: 10_000n });
  });

  it('a row still loading has no figure but its order counts; an unavailable row is flagged, never a zero', () => {
    const r = row('EXKCC', ID('a'));
    const loading = tokenEntry(r, LOADING);
    expect(loading.loading).toBe(true);
    expect(loading.cells.last.text).toBeNull();
    expect(loading.cells.orders.text).toBe('7');
    const none = tokenEntry(r, NONE);
    expect(none.unavailable).toBe(true);
    expect([none.cells.last.text, none.cells.change.text, none.cells.volume.text, none.cells.spread.text]).toEqual([null, null, null, null]);
    expect(none.sort.volume).toBeNull();
  });

  it('a market without trades: no price or change, a zero volume, the spread of a one-sided book is none but the tooltip names the side there', () => {
    const r = row('EXKCC', ID('a'));
    const e = tokenEntry(r, ok(stats(r.covenantId, { last: null, open_24h: null, change_24h_bps: null, quote_volume_24h: '0', trades_24h: 0, best_bid: null, spread_bps: null, mid: null })));
    expect(e.cells.last.text).toBeNull();
    expect(e.cells.change.text).toBeNull();
    expect(e.cells.volume.text).toBe('0.00');
    expect(e.cells.spread).toEqual({ text: null, title: 'Best bid — · best ask 0.0252 (EXKCC / KAS)' });
  });

  it('never cuts a digit: a large price or volume turns compact with the exact figure in the tooltip, a dust volume reads <0.01', () => {
    expect(priceCell({ num: 25_300_000_123n, den: 1000n }, 4, 'X / KAS')).toEqual({ text: '25,300,000.1', title: '25,300,000.123 (X / KAS)' });
    expect(priceCell({ num: 25_300_000_000_123n, den: 1000n }, 4, 'X / KAS')).toEqual({ text: '25.30B', title: '25,300,000,000.123 (X / KAS)' });
    expect(volumeCell(123_456_789_000_000_000n, 8, 'KAS', null)).toEqual({ text: '1.23B', title: '1,234,567,890 KAS' });
    expect(volumeCell(12_345_678_900n, 8, 'KAS', null)).toEqual({ text: '123.46', title: '123.456789 KAS' });
    expect(volumeCell(1n, 8, 'KAS', null)).toEqual({ text: '<0.01', title: '0.00000001 KAS' });
    expect(volumeCell(5n, 0, 'T', null)).toEqual({ text: '5', title: '5 T' });
  });

  it('a negative change is red, zero is flat', () => {
    expect(changeCell(-1234)).toEqual({ text: '-12.34%', title: null, tone: 'down' });
    expect(changeCell(0)).toEqual({ text: '0.00%', title: null, tone: 'flat' });
    expect(changeCell(null).text).toBeNull();
  });

  it('a token without a registry tick shows about five significant digits; a token outside the registry is named by 8 + 8 hex', () => {
    const r = row('', ID('c'), { info: null, source: 'indexer', tick: null });
    const e = tokenEntry(r, ok(stats(r.covenantId, { last: '123456789' })));
    expect(e.cells.last.text).toBe('1.2346');
    expect(e.cells.last.title).toBe(`1.23456789 (cccccccc…cccccccc / KAS)`);
    expect(e.name).toBe('cccccccc…cccccccc');
  });
});

describe('market list pairs', () => {
  const a = row('EXKCC', ID('a'));
  const u = row('EXUSD', ID('e'), { decimals: 6, scale: 1_000_000n });
  const summary = (base: string, quote: string): PairSummaryView => ({ base, quote, direct_asks: 3, direct_bids: 1, entry_asks: 0, entry_bids: 1, conditionals: 1 });

  it('lists a pair only when both tokens are listed here and in the registry, once per orientation', () => {
    const open = row('', ID('f'), { info: null, source: 'indexer' });
    const got = listedPairs([summary(a.covenantId, u.covenantId), summary(a.covenantId, u.covenantId), summary(u.covenantId, a.covenantId), summary(a.covenantId, open.covenantId), summary(a.covenantId, ID('9'))], [a, u, open]);
    expect(got.map((p) => p.key)).toEqual([`${a.covenantId}/${u.covenantId}`, `${u.covenantId}/${a.covenantId}`]);
    expect(pairLabelOf(got[0]!)).toBe('EXKCC (aaaa…aaaa) / EXUSD (eeee…eeee)');
    expect(listedPairs(null, [a, u])).toEqual([]);
  });

  it('a pair: price and change implied through the two KAS markets, the 24 h pair volume in QUOTE, its live order counts, no spread', () => {
    const [p] = listedPairs([summary(a.covenantId, u.covenantId)], [a, u]);
    // EXKCC 0.0251 KAS (open 0.0250), EXUSD 0.5 KAS (open 0.5): 0.0502 EXUSD per EXKCC, +0.40 %
    const e = pairEntry(p!, ok(stats(a.covenantId)), ok(stats(u.covenantId, { last: '50000000', open_24h: '50000000', price_basis: '1000000', decimals: 6 })), ok({ a: 300_000_000n, b: 151_500n, fills: 2 }));
    // five significant digits, as the pair page shows it
    expect(e.cells.last).toEqual({ text: '0.050200', title: '0.0502 (EXKCC / EXUSD)' });
    expect(e.cells.change.text).toBe('+0.40%');
    expect(e.cells.volume).toEqual({ text: '0.15', title: '0.1515 EXUSD · 2 pair fills in 24 h' });
    expect(e.cells.spread.text).toBeNull();
    expect(e.cells.orders).toEqual({ text: '6', title: '3 asks · 2 bids · 1 conditional' });
    expect(e.kind).toBe('pair');
    expect(e.unavailable).toBe(false);
  });

  it('a pair is loading while a leg or its volume is, and unavailable when the indexer answered neither', () => {
    const [p] = listedPairs([summary(a.covenantId, u.covenantId)], [a, u]);
    expect(pairEntry(p!, LOADING, ok(stats(u.covenantId)), NONE).loading).toBe(true);
    const none = pairEntry(p!, NONE, NONE, NONE);
    expect(none.unavailable).toBe(true);
    expect(none.cells.last.text).toBeNull();
    expect(none.cells.orders.text).toBe('6');
  });

  it('search: either token, or BASE/QUOTE side by side (look-alike letters too)', () => {
    const k = row('KRON', ID('b'));
    const pairs = listedPairs([summary(a.covenantId, u.covenantId), summary(k.covenantId, u.covenantId), summary(u.covenantId, a.covenantId)], [a, u, k]);
    const keys = (q: string) => filterPairs(pairs, q).map((p) => `${p.base.ticker}/${p.quote.ticker}`);
    expect(keys('')).toEqual(['EXKCC/EXUSD', 'KRON/EXUSD', 'EXUSD/EXKCC']);
    expect(keys('kr0n')).toEqual(['KRON/EXUSD']);
    expect(keys('exkcc')).toEqual(['EXKCC/EXUSD', 'EXUSD/EXKCC']);
    expect(keys('exkcc/exusd')).toEqual(['EXKCC/EXUSD']);
    expect(keys('/exkcc')).toEqual(['EXUSD/EXKCC']);
    expect(keys('zzz')).toEqual([]);
  });
});

describe('market list sorting', () => {
  const entry = (key: string, name: string, s: Partial<ListEntry['sort']>): Pick<ListEntry, 'key' | 'name' | 'sort'> => ({
    key, name, sort: { last: null, change: null, volume: null, spread: null, orders: null, ...s },
  });
  const list = [
    entry('a', 'alpha', { volume: 5, change: -10, last: { num: 1n, den: 3n }, spread: 50, orders: 2 }),
    entry('b', 'bravo', { volume: 50, change: 30, last: { num: 1n, den: 2n }, spread: 10, orders: 9 }),
    entry('c', 'charlie', {}),
    entry('d', 'delta', { volume: 5, change: 0, last: { num: 2n, den: 7n }, orders: 0 }),
  ];
  const order = (key: Parameters<typeof nextSort>[1], dir: 'asc' | 'desc') => sortEntries(list, { key, dir }).map((e) => e.key);

  it('opens by 24 h volume, most traded first; ties by name; missing figures last in both directions', () => {
    expect(DEFAULT_SORT).toEqual({ key: 'volume', dir: 'desc' });
    expect(order('volume', 'desc')).toEqual(['b', 'a', 'd', 'c']);
    expect(order('volume', 'asc')).toEqual(['a', 'd', 'b', 'c']);
  });

  it('sorts by price exactly (rationals), change, spread, orders and name', () => {
    expect(order('last', 'desc')).toEqual(['b', 'a', 'd', 'c']);
    expect(order('last', 'asc')).toEqual(['d', 'a', 'b', 'c']);
    expect(order('change', 'desc')).toEqual(['b', 'd', 'a', 'c']);
    expect(order('spread', 'asc')).toEqual(['b', 'a', 'c', 'd']);
    expect(order('orders', 'desc')).toEqual(['b', 'a', 'd', 'c']);
    expect(order('market', 'asc')).toEqual(['a', 'b', 'c', 'd']);
    expect(order('market', 'desc')).toEqual(['d', 'c', 'b', 'a']);
  });

  it('a header click starts in the column\'s own direction and turns over on the second click; aria-sort follows', () => {
    expect(firstDir('market')).toBe('asc');
    expect(firstDir('spread')).toBe('asc');
    expect(firstDir('last')).toBe('desc');
    let s = nextSort(DEFAULT_SORT, 'volume');
    expect(s).toEqual({ key: 'volume', dir: 'asc' });
    s = nextSort(s, 'market');
    expect(s).toEqual({ key: 'market', dir: 'asc' });
    expect(ariaSort(s, 'market')).toBe('ascending');
    expect(ariaSort(s, 'volume')).toBe('none');
    expect(ariaSort(nextSort(s, 'market'), 'market')).toBe('descending');
  });
});

describe('runPool', () => {
  it('keeps at most `limit` calls in flight, runs every item once and survives a failing one', async () => {
    let inFlight = 0;
    let peak = 0;
    const done: number[] = [];
    await runPool([1, 2, 3, 4, 5, 6, 7], 3, async (n) => {
      inFlight++;
      peak = Math.max(peak, inFlight);
      await new Promise((r) => setTimeout(r, 5));
      inFlight--;
      if (n === 3) throw new Error('boom');
      done.push(n);
    });
    expect(peak).toBe(3);
    expect(done.sort()).toEqual([1, 2, 4, 5, 6, 7]);
  });

  it('takes no new item once aborted', async () => {
    const ctl = new AbortController();
    const seen: number[] = [];
    await runPool([1, 2, 3, 4], 1, async (n) => {
      seen.push(n);
      if (n === 2) ctl.abort();
    }, ctl.signal);
    expect(seen).toEqual([1, 2]);
  });
});
