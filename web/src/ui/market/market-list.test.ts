import { describe, expect, it } from 'vitest';
import type { PairSummaryView, StatsView } from '../../data/indexer-types';
import type { TokenInfo } from '../../kob/registry';
import {
  DEFAULT_LIST_PAGE_SIZE, keepOrder, listPageSizeOf, pageWindow, pairHasDecimals,
  ariaSort, changeCell, DEFAULT_SORT, filterPairs, firstDir, kascovEntry, listedPairs, LOADING, nextSort, NONE, pairEntry, pairLabelOf, priceCell, runPool, settled, sortEntries,
  depthCell, marketCapCell, pairDepthCell, supplyOf, tokenEntry, volumeCell, type ListEntry, type Slot,
} from './market-list';
import type { KascovMeta, TokenRow } from './token-model';

const ID = (c: string) => c.repeat(64);

/** A registry token row (8 decimals, tick 0.0001 KAS unless given). */
function row(ticker: string, id: string, over: Partial<TokenRow> = {}): TokenRow {
  return {
    covenantId: id, ticker, name: `${ticker} coin`, source: 'registry', info: { covenantId: id, ticker } as unknown as TokenInfo, index: null, powers: [],
    issuerControl: false, facts: [], templateId: 'kcc20-ref-8x8', badges: [], tradable: true, reason: null, indexerProblems: [], openAsks: 3, openBids: 4, decimals: 8,
    scale: 100_000_000n, tick: 10_000n, ...over,
  };
}

/** `/v1/stats` of a token (prices sompi per whole token of 8 decimals). */
function stats(token: string, over: Partial<StatsView> = {}): StatsView {
  return {
    token, price_basis: '100000000', decimals: 8, ts: 1_700_000_000_000, last: '2510000', last_ts: 1, last_side: 'buy', open_24h: '2500000', high_24h: '2600000',
    low_24h: '2400000', change_24h_bps: 40, volume_24h: '1500000000', quote_volume_24h: '37650000', trades_24h: 7, best_bid: '2500000', best_ask: '2520000',
    mid: '2510000', spread_bps: 79, open_asks: 3, open_bids: 4, depth_2pct_quote: '120000000000', ...over,
  };
}
const ok = <T,>(value: T): Slot<T> => ({ state: 'ok', value });

describe('settled', () => {
  it('a read in flight without an answer is not settled; one with an answer, or one that ended without any, is', () => {
    expect(settled({ data: undefined, loading: true })).toBe(false);
    expect(settled({ data: null, loading: true })).toBe(false);
    expect(settled({ data: { a: 1 }, loading: true })).toBe(true);
    expect(settled({ data: null, loading: false })).toBe(true);
    expect(settled({ data: undefined, loading: false })).toBe(true);
  });
});

describe('the list\'s rows', () => {
  it('a figure that arrives moves no row; a row that leaves goes; a new row follows', () => {
    expect(keepOrder(['a', 'b', 'c'], ['c', 'b', 'a'])).toEqual(['a', 'b', 'c']);
    expect(keepOrder(['a', 'b', 'c'], ['c', 'a'])).toEqual(['a', 'c']);
    expect(keepOrder(['a', 'b'], ['d', 'b', 'c', 'a'])).toEqual(['a', 'b', 'd', 'c']);
    expect(keepOrder([], ['b', 'a'])).toEqual(['b', 'a']);
  });
  it('a stored page size is one of the choices, else the default', () => {
    expect(listPageSizeOf('50')).toBe(50);
    expect(listPageSizeOf('100')).toBe(100);
    for (const raw of [null, undefined, '', '7', '20px']) expect(listPageSizeOf(raw)).toBe(DEFAULT_LIST_PAGE_SIZE);
    expect(listPageSizeOf(null, 50)).toBe(50);
    expect(listPageSizeOf('100', 50)).toBe(100);
  });
  it('a page holds exactly its rows; the last one what is left', () => {
    expect(pageWindow(48, 20, 0)).toEqual({ page: 0, pages: 3, from: 0, to: 20 });
    expect(pageWindow(48, 20, 1)).toEqual({ page: 1, pages: 3, from: 20, to: 40 });
    expect(pageWindow(48, 20, 2)).toEqual({ page: 2, pages: 3, from: 40, to: 48 });
    expect(pageWindow(100, 50, 1)).toEqual({ page: 1, pages: 2, from: 50, to: 100 });
    expect(pageWindow(100, 100, 0)).toEqual({ page: 0, pages: 1, from: 0, to: 100 });
  });
  it('a list that got shorter shows its last page; an empty list has one page; no page before the first', () => {
    expect(pageWindow(30, 20, 4)).toEqual({ page: 1, pages: 2, from: 20, to: 30 });
    expect(pageWindow(0, 20, 3)).toEqual({ page: 0, pages: 1, from: 0, to: 0 });
    expect(pageWindow(30, 20, -1)).toEqual({ page: 0, pages: 2, from: 0, to: 20 });
  });
});

describe('market list cells', () => {
  it('a token row: last price in KAS at the display decimals (5 significant digits, at most 6) with the exact price and KAS per TOKEN unit in the tooltip, change, KAS volume, spread, depth within 2 % (with the order counts in its tooltip); no supply, no market cap', () => {
    const r = row('EXKCC', ID('a'));
    const e = tokenEntry(r, ok(stats(r.covenantId)));
    expect(e.cells.last).toEqual({ text: '0.025100', title: '0.0251 (KAS per EXKCC)' });
    expect(e.cells.change).toEqual({ text: '+0.40%', title: null, tone: 'up' });
    expect(e.cells.volume).toEqual({ text: '0.38', title: '0.3765 KAS · 7 trades in 24 h' });
    expect(e.cells.spread).toEqual({ text: '0.79%', title: 'Best bid 0.025000 · best ask 0.025200 (KAS per EXKCC)' });
    expect(e.cells.depth).toEqual({ text: '1,200.00', title: '1,200.00 KAS · 3 asks · 4 bids' });
    expect(e.cells.mcap).toEqual({ text: null, title: null });
    expect(e.sort.depth).toBe(1200);
    expect(e.sort.mcap).toBeNull();
    expect(e.loading).toBe(false);
    expect(e.unavailable).toBe(false);
    expect(e.sort.volume).toBeCloseTo(0.3765);
    expect(e.sort.change).toBe(40);
    expect(e.sort.last).toEqual({ num: 251n, den: 10_000n });
  });

  it('a token with unknown decimals (no registry entry, no declaration; the indexer reports 0) shows the price per whole token on the assumed 8 decimals, as its page does', () => {
    const r = row('', ID('c'), { decimals: null, scale: null, tick: null, source: 'indexer', info: null });
    const e = tokenEntry(r, ok(stats(r.covenantId, { decimals: 0, last: '2877252083', mid: '2877252083', best_bid: '2877000000', best_ask: '2878000000' })));
    expect(e.cells.last.text).toMatch(/^28[.]77/);
    expect(e.sort.last).toEqual({ num: 2877252083n, den: 100_000_000n });
    // the same figures as a row that knows its 8 decimals
    expect(e.sort.last).toEqual(tokenEntry(row('X', ID('f')), ok(stats(ID('f'), { last: '2877252083', mid: '2877252083' }))).sort.last);
    // a known count wins over the indexer's 0, and a positive count the indexer reports is used when the row has none
    const at = (decimals: number | null, reported: number) => tokenEntry(row('X', ID('d'), { decimals }), ok(stats(ID('d'), { decimals: reported, last: '28772' }))).sort.last;
    expect(at(3, 0)).toEqual(at(null, 3));
    expect(at(3, 0)).not.toEqual(at(null, 0));
  });

  it('a row still loading has no figure; an unavailable row is flagged, never a zero', () => {
    const r = row('EXKCC', ID('a'));
    const loading = tokenEntry(r, LOADING);
    expect(loading.loading).toBe(true);
    expect(loading.cells.last.text).toBeNull();
    expect(loading.cells.depth.text).toBeNull();
    const none = tokenEntry(r, NONE);
    expect(none.unavailable).toBe(true);
    expect([none.cells.last.text, none.cells.change.text, none.cells.volume.text, none.cells.mcap.text, none.cells.spread.text, none.cells.depth.text]).toEqual([null, null, null, null, null, null]);
    expect(none.sort.volume).toBeNull();
  });

  it('a row whose 24 h change rests on 1 or 2 trades shows it muted with the count; the same row with 7 trades does not', () => {
    const r = row('EXKCC', ID('a'));
    const thin = tokenEntry(r, ok(stats(r.covenantId, { change_24h_bps: 90_000, trades_24h: 2 })));
    expect(thin.cells.change).toEqual({ text: '+900.00%', title: 'Based on 2 trades in 24 h', tone: 'flat', thin: true });
    expect(tokenEntry(r, ok(stats(r.covenantId, { change_24h_bps: 90_000 }))).cells.change).toEqual({ text: '+900.00%', title: null, tone: 'up' });
  });

  it('a market without trades: no price or change, a zero volume, the spread of a one-sided book is none but the tooltip names the side there', () => {
    const r = row('EXKCC', ID('a'));
    const e = tokenEntry(r, ok(stats(r.covenantId, { last: null, open_24h: null, change_24h_bps: null, quote_volume_24h: '0', trades_24h: 0, best_bid: null, spread_bps: null, mid: null })));
    expect(e.cells.last.text).toBeNull();
    expect(e.cells.change.text).toBeNull();
    expect(e.cells.volume.text).toBe('0.00');
    expect(e.cells.spread).toEqual({ text: null, title: 'Best bid — · best ask 0.025200 (KAS per EXKCC)' });
  });

  it('never cuts a digit: a large price or volume turns compact with the exact figure in the tooltip, a dust volume reads <0.01', () => {
    expect(priceCell({ num: 25_300_000_123n, den: 1000n }, 4, 'KAS per X')).toEqual({ text: '25,300,000.1', title: '25,300,000.123 (KAS per X)' });
    expect(priceCell({ num: 25_300_000_000_123n, den: 1000n }, 4, 'KAS per X')).toEqual({ text: '25.30B', title: '25,300,000,000.123 (KAS per X)' });
    expect(volumeCell(123_456_789_000_000_000n, 8, 'KAS', null)).toEqual({ text: '1.23B', title: '1,234,567,890 KAS' });
    expect(volumeCell(12_345_678_900n, 8, 'KAS', null)).toEqual({ text: '123.46', title: '123.456789 KAS' });
    expect(volumeCell(1n, 8, 'KAS', null)).toEqual({ text: '<0.01', title: '0.00000001 KAS' });
    expect(volumeCell(5n, 0, 'T', null)).toEqual({ text: '5', title: '5 T' });
  });

  it('a negative change is red, zero is flat', () => {
    expect(changeCell(-1234)).toEqual({ text: '-12.34%', title: null, tone: 'down' });
    expect(changeCell(0)).toEqual({ text: '0.00%', title: null, tone: 'flat' });
    expect(changeCell(null).text).toBeNull();
    // fewer than 3 trades in the window: muted, the count in the tooltip; 3 or none reported: as before
    expect(changeCell(90_000, 2)).toEqual({ text: '+900.00%', title: 'Based on 2 trades in 24 h', tone: 'flat', thin: true });
    expect(changeCell(0, 1)).toEqual({ text: '0.00%', title: 'Based on 1 trade in 24 h', tone: 'flat', thin: true });
    expect(changeCell(90_000, 3)).toEqual({ text: '+900.00%', title: null, tone: 'up' });
    expect(changeCell(90_000, null)).toEqual({ text: '+900.00%', title: null, tone: 'up' });
    expect(changeCell(90_000)).toEqual({ text: '+900.00%', title: null, tone: 'up' });
  });

  it('a token without a registry tick shows about five significant digits; a token outside the registry is named by 8 + 8 hex', () => {
    const r = row('', ID('c'), { info: null, source: 'indexer', tick: null });
    const e = tokenEntry(r, ok(stats(r.covenantId, { last: '123456789' })));
    expect(e.cells.last.text).toBe('1.2346');
    expect(e.cells.last.title).toBe(`1.23456789 (KAS per cccccccc…cccccccc)`);
    expect(e.name).toBe('cccccccc…cccccccc');
  });
});

describe('market list pairs', () => {
  const a = row('EXKCC', ID('a'));
  const u = row('EXUSD', ID('e'), { decimals: 6, scale: 1_000_000n });
  const summary = (base: string, quote: string): PairSummaryView => ({ base, quote, direct_asks: 3, direct_bids: 1, entry_asks: 0, entry_bids: 1, conditionals: 1 });
  const pair = (base: TokenRow, quote: TokenRow) => ({ base: base.covenantId, quote: quote.covenantId });

  it('lists the favourite pairs in their order, each as it was favoured, with the order counts of both orientations (the flipped pair turns them round)', () => {
    const got = listedPairs([pair(u, a), pair(a, u)], [a, u], [summary(a.covenantId, u.covenantId)]);
    expect(got.map((p) => p.key)).toEqual([`${u.covenantId}/${a.covenantId}`, `${a.covenantId}/${u.covenantId}`]);
    // the one listed orientation is counted under either way round: EXKCC/EXUSD has 3 asks and 1 bid, so EXUSD/EXKCC has 1 ask and 3 bids
    expect(got.map((p) => p.summary && [p.summary.direct_asks, p.summary.direct_bids])).toEqual([[1, 3], [3, 1]]);
    expect(listedPairs([pair(a, u)], [a, u], [])[0]!.summary).toBeNull();
    expect(pairLabelOf(got[1]!)).toBe('EXKCC (aaaa…aaaa) / EXUSD (eeee…eeee)');
    expect(listedPairs([], [a, u], [summary(a.covenantId, u.covenantId)])).toEqual([]);
  });

  it('a favourite no list names stays, by its id and with no figures: its star can be taken off', () => {
    const [p] = listedPairs([{ base: a.covenantId, quote: ID('b') }], [a]);
    expect(pairLabelOf(p!)).toBe(`EXKCC (aaaa…aaaa) / ${'b'.repeat(8)}…${'b'.repeat(8)}`);
    expect(pairHasDecimals(p!)).toBe(false);
    const e = pairEntry(p!, ok(stats(a.covenantId)), ok(stats(ID('b'))), NONE, NONE);
    expect(e.loading).toBe(false);
    expect(e.cells.last.text).toBeNull();
    expect(e.cells.change.text).toBeNull();
  });

  it('an open-list token takes the decimals of the scale its first order set; a scale that is no power of ten leaves it without', () => {
    const view = (market_scale: number) => ({ market_scale }) as unknown as TokenRow['index'];
    const open = row('', ID('f'), { info: null, source: 'indexer', decimals: null, scale: null, index: view(1_000_000) });
    const [p] = listedPairs([pair(a, open)], [a, open]);
    expect(p!.quote.decimals).toBe(6);
    expect(pairHasDecimals(listedPairs([pair(a, open)], [a, { ...open, index: view(3) }])[0]!)).toBe(false);
  });

  it('a pair: price and change implied through the two KAS markets, the 24 h pair volume in QUOTE, its live order counts, no spread', () => {
    const [p] = listedPairs([pair(a, u)], [a, u]);
    // EXKCC 0.0251 KAS (open 0.0250), EXUSD 0.5 KAS (open 0.5): 0.0502 EXUSD per EXKCC, +0.40 %
    const e = pairEntry(p!, ok(stats(a.covenantId)), ok(stats(u.covenantId, { last: '50000000', open_24h: '50000000', price_basis: '1000000', decimals: 6 })), ok({ a: 300_000_000n, b: 151_500n, fills: 2 }));
    // five significant digits, as the pair page shows it
    expect(e.cells.last).toEqual({ text: '0.050200', title: '0.0502 (EXUSD per EXKCC)' });
    expect(e.cells.change.text).toBe('+0.40%');
    expect(e.cells.volume).toEqual({ text: '0.15', title: '0.1515 EXUSD · 2 pair fills in 24 h' });
    expect(e.cells.spread.text).toBeNull();
    // no depth read yet (the book is a separate call): none, not a zero
    expect(e.cells.depth).toEqual({ text: null, title: null });
    expect(e.cells.mcap).toEqual({ text: null, title: null });
    expect(e.kind).toBe('pair');
    expect(e.unavailable).toBe(false);
  });

  it('a pair is loading while a leg or its volume is, and unavailable when the indexer answered neither', () => {
    const [p] = listedPairs([pair(a, u)], [a, u]);
    expect(pairEntry(p!, LOADING, ok(stats(u.covenantId)), NONE).loading).toBe(true);
    const none = pairEntry(p!, NONE, NONE, NONE);
    expect(none.unavailable).toBe(true);
    expect(none.cells.last.text).toBeNull();
    expect(none.cells.depth.text).toBeNull();
  });

  it('search: either token, or BASE/QUOTE side by side (look-alike letters too)', () => {
    const k = row('KRON', ID('b'));
    const pairs = listedPairs([pair(a, u), pair(k, u), pair(u, a)], [a, u, k]);
    const keys = (q: string) => filterPairs(pairs, q).map((p) => `${p.base.ticker}/${p.quote.ticker}`);
    expect(keys('')).toEqual(['EXKCC/EXUSD', 'KRON/EXUSD', 'EXUSD/EXKCC']);
    expect(keys('kr0n')).toEqual(['KRON/EXUSD']);
    expect(keys('exkcc')).toEqual(['EXKCC/EXUSD', 'EXUSD/EXKCC']);
    expect(keys('exkcc/exusd')).toEqual(['EXKCC/EXUSD']);
    expect(keys('/exkcc')).toEqual(['EXUSD/EXKCC']);
    expect(keys('zzz')).toEqual([]);
  });
});

describe('rows only kascov.io names', () => {
  it('say "no orders", fetch nothing (never loading or unavailable) and have no figure', () => {
    const e = kascovEntry(row('ROCKET', ID('9'), { source: 'kascov', info: null, openAsks: null, openBids: null, decimals: null, scale: null, tick: null }));
    expect(e).toMatchObject({ key: ID('9'), kind: 'token', loading: false, unavailable: false, noOrders: true });
    expect(e.cells.depth.text).toBe('no orders');
    expect(e.cells.depth.title).toContain('kascov.io');
    for (const k of ['last', 'change', 'volume', 'mcap', 'spread'] as const) expect(e.cells[k]).toEqual({ text: null, title: null });
    expect(e.sort).toEqual({ last: null, change: null, volume: null, mcap: null, spread: null, depth: null });
  });
});

describe('market list sorting', () => {
  const entry = (key: string, name: string, s: Partial<ListEntry['sort']>): Pick<ListEntry, 'key' | 'name' | 'sort'> => ({
    key, name, sort: { last: null, change: null, volume: null, mcap: null, spread: null, depth: null, ...s },
  });
  const list = [
    entry('a', 'alpha', { volume: 5, change: -10, last: { num: 1n, den: 3n }, spread: 50, depth: 2, mcap: 7 }),
    entry('b', 'bravo', { volume: 50, change: 30, last: { num: 1n, den: 2n }, spread: 10, depth: 9, mcap: 700 }),
    entry('c', 'charlie', {}),
    entry('d', 'delta', { volume: 5, change: 0, last: { num: 2n, den: 7n }, depth: 0 }),
  ];
  const order = (key: Parameters<typeof nextSort>[1], dir: 'asc' | 'desc') => sortEntries(list, { key, dir }).map((e) => e.key);

  it('opens by 24 h volume, most traded first; ties by name; missing figures last in both directions', () => {
    expect(DEFAULT_SORT).toEqual({ key: 'volume', dir: 'desc' });
    expect(order('volume', 'desc')).toEqual(['b', 'a', 'd', 'c']);
    expect(order('volume', 'asc')).toEqual(['a', 'd', 'b', 'c']);
  });

  it('sorts by price exactly (rationals), change, spread, market cap, depth and name', () => {
    expect(order('last', 'desc')).toEqual(['b', 'a', 'd', 'c']);
    expect(order('last', 'asc')).toEqual(['d', 'a', 'b', 'c']);
    expect(order('change', 'desc')).toEqual(['b', 'd', 'a', 'c']);
    expect(order('spread', 'asc')).toEqual(['b', 'a', 'c', 'd']);
    expect(order('depth', 'desc')).toEqual(['b', 'a', 'd', 'c']);
    // a missing figure is last in both directions; a zero depth is a figure
    expect(order('depth', 'asc')).toEqual(['d', 'a', 'b', 'c']);
    expect(order('mcap', 'desc')).toEqual(['b', 'a', 'c', 'd']);
    expect(order('market', 'asc')).toEqual(['a', 'b', 'c', 'd']);
    expect(order('market', 'desc')).toEqual(['d', 'c', 'b', 'a']);
  });

  it('a header click starts in the column\'s own direction and turns over on the second click; aria-sort follows', () => {
    expect(firstDir('market')).toBe('asc');
    expect(firstDir('spread')).toBe('asc');
    expect(firstDir('last')).toBe('desc');
    expect(firstDir('mcap')).toBe('desc');
    expect(firstDir('depth')).toBe('desc');
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

describe('market cap', () => {
  const meta = (over: Partial<KascovMeta> = {}): KascovMeta => ({
    ticker: null, name: null, usedTicker: false, usedName: false, verified: true, tickerDiffers: false, decimalsDiffers: false, decimals: 8, imageHash: null, imageUrl: null,
    supply: 10n ** 16n, ...over,
  });
  /** a token of 8 decimals whose supply, as kascov reports it, is 100,000,000 tokens */
  const kascovRow = (over: Partial<KascovMeta> = {}): TokenRow => row('EXKCC', ID('a'), { info: null, source: 'indexer', kascov: meta(over) });

  it('is the last trade price x the supply, in KAS, with the supply and its source in the tooltip; sortable by its value', () => {
    // 0.0251 KAS x 100,000,000 tokens = 2,510,000 KAS
    const e = tokenEntry(kascovRow(), ok(stats(ID('a'))));
    expect(e.cells.mcap).toEqual({
      text: '2,510,000.00',
      title: '2,510,000.00 KAS · last price × 100,000,000 EXKCC (supply as kascov.io reports it, not checked by KOB)',
    });
    expect(e.sort.mcap).toBeCloseTo(2_510_000);
  });

  it('turns compact when a cell would be too wide, and divides by the token\'s own decimals', () => {
    const big = tokenEntry(kascovRow({ supply: 10n ** 22n }), ok(stats(ID('a'))));
    expect(big.cells.mcap.text).toBe('2.51T');
    expect(big.cells.mcap.title).toContain('2,510,000,000,000 KAS');
    // 3 decimals: a supply of 1,000,000.000 tokens (10^9 base units) at 0.0251 KAS is 25,100 KAS
    const r = row('EXKCC', ID('a'), { info: null, source: 'indexer', decimals: 3, scale: 1000n, kascov: meta({ supply: 10n ** 9n, decimals: 3 }) });
    expect(tokenEntry(r, ok(stats(ID('a'), { price_basis: '1000', decimals: 3, last: '2510000' }))).sort.mcap).toBeCloseTo(25_100);
  });

  it('is empty without a last trade, without a supply, and when the supply is not a sound fact', () => {
    const none = { text: null, title: null };
    expect(tokenEntry(kascovRow(), ok(stats(ID('a'), { last: null }))).cells.mcap).toEqual(none);
    // no supply on the row (a token kascov does not list), kascov does not vouch for the token, or it disagrees about the decimals, or the token can still be issued
    expect(tokenEntry(row('EXKCC', ID('a')), ok(stats(ID('a')))).cells.mcap).toEqual(none);
    expect(tokenEntry(kascovRow({ verified: false }), ok(stats(ID('a')))).cells.mcap).toEqual(none);
    expect(tokenEntry(kascovRow({ decimalsDiffers: true }), ok(stats(ID('a')))).cells.mcap).toEqual(none);
    expect(tokenEntry(kascovRow({ supply: null }), ok(stats(ID('a')))).cells.mcap).toEqual(none);
    expect(marketCapCell(null, { units: 5n, source: 'kascov' }, 8, 'X')).toEqual({ cell: none, value: null });
  });

  it('takes the supply of a registry genesis record that cannot mint more before kascov\'s figure', () => {
    const genesis = { txid: 'ab'.repeat(32), daa_score: 1, outputs: [0], supply: 5_000_000_000, minter_outputs: [] as number[] };
    const info = (g: object, verified = true) => ({ json: { genesis_verified: verified, genesis: g } }) as unknown as TokenInfo;
    const rowOf = (g: object, verified = true) => row('EXKCC', ID('a'), { info: info(g, verified), kascov: meta({ supply: 7n }) });
    expect(supplyOf(rowOf(genesis))).toEqual({ units: 5_000_000_000n, source: 'registry' });
    // a way to mint more (an allowance, a minter output, a live minter) or an unverified record: not a fixed supply, kascov's figure (if it vouches) is used instead
    expect(supplyOf(rowOf({ ...genesis, mint_allowance: 1 }))).toEqual({ units: 7n, source: 'kascov' });
    expect(supplyOf(rowOf({ ...genesis, minter_outputs: [1] }))).toEqual({ units: 7n, source: 'kascov' });
    expect(supplyOf(rowOf({ ...genesis, live_minters: ['cd'.repeat(32) + ':1'] }))).toEqual({ units: 7n, source: 'kascov' });
    expect(supplyOf(rowOf(genesis, false))).toEqual({ units: 7n, source: 'kascov' });
    const cell = marketCapCell({ num: 251n, den: 10_000n }, { units: 5_000_000_000n, source: 'registry' }, 8, 'EXKCC').cell;
    // 0.0251 KAS x 50 tokens = 1.255 KAS
    expect(cell).toEqual({ text: '1.26', title: '1.255 KAS · last price × 50 EXKCC (fixed supply, from the verified genesis record in the registry)' });
  });
});

describe('depth within 2 %', () => {
  it('a token: the KAS the book offers near its mid, with the order counts in the tooltip; a zero is a figure, an empty book (null) or an older indexer (absent) is none', () => {
    const r = row('EXKCC', ID('a'));
    expect(tokenEntry(r, ok(stats(ID('a'), { depth_2pct_quote: '0' }))).cells.depth).toEqual({ text: '0.00', title: '0.00 KAS · 3 asks · 4 bids' });
    expect(tokenEntry(r, ok(stats(ID('a'), { depth_2pct_quote: null }))).cells.depth).toEqual({ text: null, title: null });
    const { depth_2pct_quote: _gone, ...older } = stats(ID('a'));
    expect(tokenEntry(r, ok(older as StatsView)).cells.depth).toEqual({ text: null, title: null });
    expect(tokenEntry(r, ok(stats(ID('a'), { depth_2pct_quote: 'oops' }))).cells.depth).toEqual({ text: null, title: null });
    // without order counts there is no note
    expect(depthCell(150_000_000n, null)).toEqual({ cell: { text: '1.50', title: '1.50 KAS' }, value: 1.5 });
  });

  it('a pair: the QUOTE value from its book, a lower bound marked "≥"; the pair table has no market cap', () => {
    const a = row('EXKCC', ID('a'));
    const u = row('EXUSD', ID('e'), { decimals: 6, scale: 1_000_000n });
    const wanted = [{ base: a.covenantId, quote: u.covenantId }];
    const [p] = listedPairs(wanted, [a, u], [{ ...wanted[0]!, direct_asks: 3, direct_bids: 1, entry_asks: 0, entry_bids: 1, conditionals: 1 }]);
    const e = pairEntry(p!, ok(stats(a.covenantId)), ok(stats(u.covenantId)), NONE, ok({ quote: 2_500_000n, atLeast: false }));
    expect(e.cells.depth).toEqual({ text: '2.50', title: '2.50 EXUSD · 3 asks · 2 bids · 1 conditional' });
    // a pair without live orders: no counts to name
    expect(pairEntry(listedPairs(wanted, [a, u])[0]!, ok(stats(a.covenantId)), ok(stats(u.covenantId)), NONE, ok({ quote: 2_500_000n, atLeast: false })).cells.depth.title).toBe('2.50 EXUSD');
    expect(e.sort.depth).toBeCloseTo(2.5);
    expect(e.cells.mcap.text).toBeNull();
    expect(pairDepthCell({ quote: 2_500_000n, atLeast: true }, 6, 'EXUSD', null)).toEqual({ text: '≥2.50', title: '2.50 EXUSD · at least: the book is deeper than the levels read' });
    // the depth is still loading: the pair row is
    expect(pairEntry(p!, ok(stats(a.covenantId)), ok(stats(u.covenantId)), NONE, LOADING).loading).toBe(true);
  });
});
