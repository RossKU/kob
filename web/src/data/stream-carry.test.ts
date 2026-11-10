// What the page read from the indexer, kept for standalone mode (data/stream-carry.ts): the recording and the merging with what the node saw.
import { describe, expect, it } from 'vitest';
import type { CandleView, CandlesView, IndexerTokenView, StatsView, TradeView, TradesView } from './indexer-types';
import { IndexerCarry, mergeCandles, mergeStats, mergeTokens, mergeTrades } from './stream-carry';

const TOKEN = '70'.repeat(32);
const OTHER = '71'.repeat(32);
const NOW = Date.parse('2026-11-11T12:00:00Z');
const bar = (t: number, o: string, h: string, l: string, c: string, volume: string, trades = 1): CandleView => ({ t, o, h, l, c, volume, quote_volume: volume, trades });
const candles = (items: CandleView[], basis = '1000', interval: CandlesView['interval'] = '1m'): CandlesView => ({ token: TOKEN, interval, price_basis: basis, items });
const trade = (id: number, txid: string, price: string): TradeView => ({ id, txid, ts: NOW, daa: id, price, amount: '10', quote: '10', side: 'buy', fills: 1, confirmations: null, settled: false });
const trades = (items: TradeView[]): TradesView => ({ token: TOKEN, price_basis: '1000', items, next_cursor: null });
const tokenRow = (id: string, over: Partial<IndexerTokenView> = {}): IndexerTokenView => ({
  ticker: '', covenant_id: id, template_hash: 'ab'.repeat(32), extension_commitment: null, decimals: null, open_asks: 3, open_bids: 2, market_scale: 1_000_000, ...over,
});
const stats = (over: Partial<StatsView> = {}): StatsView => ({
  token: TOKEN, price_basis: '1000', ts: NOW, last: '200', last_ts: NOW - 1000, last_side: 'sell', open_24h: '180', high_24h: '210', low_24h: '170', change_24h_bps: 1111, volume_24h: '500', quote_volume_24h: '500',
  trades_24h: 7, best_bid: null, best_ask: null, mid: null, spread_bps: null, open_asks: 0, open_bids: 0, ...over,
});
const noFigures: Partial<StatsView> = { last: null, last_ts: null, last_side: null, open_24h: null, high_24h: null, low_24h: null, change_24h_bps: null, volume_24h: '0', quote_volume_24h: '0', trades_24h: 0 };

describe('recording', () => {
  it('keeps the token list as the indexer gave it, with when it was read', () => {
    let now = NOW;
    const c = new IndexerCarry(() => now);
    expect(c.tokens()).toEqual([]);
    expect(c.tokenAt(TOKEN)).toBeNull();
    c.recordTokens([tokenRow(TOKEN, { ticker: 'TUSD', decimals: 6 })]);
    now += 60_000;
    expect(c.token(TOKEN)).toMatchObject({ ticker: 'TUSD', decimals: 6 });
    expect(c.tokenAt(TOKEN.toUpperCase())).toBe(NOW);
    expect(c.tokenAt(OTHER)).toBeNull();
    expect(c.scaleOf(TOKEN)).toBe(1_000_000);
    expect(c.basisOf(TOKEN)).toBe(1_000_000n);
    // a later read replaces it
    c.recordTokens([tokenRow(OTHER)]);
    expect(c.token(TOKEN)).toBeNull();
  });

  it('facts for the stream book: the program, commitment and scale of an unambiguous row; none for an ambiguous or unscaled one', () => {
    const c = new IndexerCarry(() => NOW);
    c.recordTokens([tokenRow(TOKEN, { extension_commitment: 'cd'.repeat(32) }), tokenRow(OTHER, { program_ambiguous: true, template_hash: null }), tokenRow('72'.repeat(32), { market_scale: null, scale: null })]);
    expect(c.factsOf(TOKEN)).toEqual({ templateHash: 'ab'.repeat(32), extensionCommitment: 'cd'.repeat(32), scale: 1_000_000n });
    expect(c.factsOf(OTHER)).toBeNull();
    expect(c.factsOf('72'.repeat(32))).toMatchObject({ scale: null });
    expect(c.scaleOf('72'.repeat(32))).toBeNull();
    // a scale that is no power of ten is not a scale
    c.recordTokens([tokenRow(TOKEN, { market_scale: 1234 })]);
    expect(c.scaleOf(TOKEN)).toBeNull();
  });

  it('candles: a full load replaces the series, a load from a bucket replaces the bars from there on; the price basis is the indexer\'s', () => {
    const c = new IndexerCarry(() => NOW);
    expect(c.marketAt(TOKEN)).toBeNull();
    c.recordCandles(TOKEN, { interval: '1m' }, candles([bar(60_000, '1', '2', '1', '2', '5'), bar(120_000, '2', '3', '2', '3', '6')]));
    expect(c.marketAt(TOKEN)).toBe(NOW);
    expect(c.basisOf(TOKEN)).toBe(1000n);
    c.recordCandles(TOKEN, { interval: '1m', from: 120_000 }, candles([bar(120_000, '2', '4', '2', '4', '9'), bar(180_000, '4', '4', '4', '4', '1')]));
    expect(c.candles(TOKEN, '1m').map((b) => [b.t, b.h])).toEqual([[60_000, '2'], [120_000, '4'], [180_000, '4']]);
    // another interval is its own series
    expect(c.candles(TOKEN, '1h')).toEqual([]);
    // a full load again: only what it lists
    c.recordCandles(TOKEN, { interval: '1m' }, candles([bar(180_000, '4', '4', '4', '4', '1')]));
    expect(c.candles(TOKEN, '1m').map((b) => b.t)).toEqual([180_000]);
    // another price basis: nothing priced differently is mixed in
    c.recordTrades(TOKEN, undefined, trades([trade(1, 'aa', '5')]));
    c.recordStats(TOKEN, stats({ price_basis: '100' }));
    expect(c.basisOf(TOKEN)).toBe(100n);
    expect(c.candles(TOKEN, '1m')).toEqual([]);
    expect(c.trades(TOKEN)).toEqual([]);
  });

  it('trades: the newest page is kept, a page read further back is not', () => {
    const c = new IndexerCarry(() => NOW);
    c.recordTrades(TOKEN, undefined, trades([trade(9, 'a9', '5')]));
    c.recordTrades(TOKEN, { before: 9 }, trades([trade(1, 'a1', '5')]));
    expect(c.trades(TOKEN).map((t) => t.id)).toEqual([9]);
    c.recordTrades(TOKEN, undefined, null);
    expect(c.trades(TOKEN)).toHaveLength(1);
  });

  it('parks a store per key and forgets the oldest beyond a bound', () => {
    const c = new IndexerCarry(() => NOW);
    const s = { a: 1 };
    c.park('x', s);
    expect(c.parked('x')).toBe(s);
    expect(c.parked('y')).toBeNull();
    for (let i = 0; i < 30; i++) c.park(`k${i}`, i);
    expect(c.parked('x')).toBeNull();
    expect(c.parked('k29')).toBe(29);
  });
});

describe('merging with what the node saw', () => {
  it('candles: a bucket in both opens where the carried one did, closes where the node one did and adds the volumes; the window rules hold', () => {
    const carried = [bar(60_000, '10', '12', '9', '11', '5', 2), bar(120_000, '11', '13', '10', '12', '6', 3)];
    const seen = candles([bar(120_000, '15', '20', '14', '19', '4', 1), bar(180_000, '19', '19', '19', '19', '1', 1)]);
    const out = mergeCandles(carried, seen, { interval: '1m' } as never);
    expect(out.items).toEqual([carried[0], { t: 120_000, o: '11', h: '20', l: '10', c: '19', volume: '10', quote_volume: '10', trades: 4 }, seen.items[1]]);
    expect(mergeCandles(carried, seen, { interval: '1m', from: 120_000 } as never).items.map((b) => b.t)).toEqual([120_000, 180_000]);
    expect(mergeCandles(carried, seen, { interval: '1m', to: 180_000 } as never).items.map((b) => b.t)).toEqual([60_000, 120_000]);
    expect(mergeCandles(carried, seen, { interval: '1m', limit: 2 } as never).items.map((b) => b.t)).toEqual([120_000, 180_000]);
    expect(mergeCandles(carried, seen, { interval: '1m', from: 60_000, limit: 2 } as never).items.map((b) => b.t)).toEqual([60_000, 120_000]);
  });

  it('trades: the node ones first (newest), then the carried ones; a transaction in both is the node one; cut at the limit', () => {
    const seen = trades([trade(3, 'tx3', '7'), trade(2, 'tx2', '6')]);
    const carried = [trade(900, 'tx2', '5'), trade(899, 'tx1', '4'), trade(898, 'tx0', '3')];
    expect(mergeTrades(carried, seen, 10).items.map((t) => [t.txid, t.price])).toEqual([['tx3', '7'], ['tx2', '6'], ['tx1', '4'], ['tx0', '3']]);
    expect(mergeTrades(carried, seen, 3).items.map((t) => t.txid)).toEqual(['tx3', 'tx2', 'tx1']);
  });

  it('24 h figures: the last price is the node newest fill else the carried one; the window adds the two parts until 24 h after the carried read', () => {
    const carried = stats();
    const same = mergeStats(carried, stats({ ...noFigures, ts: NOW + 60_000 }), NOW + 60_000);
    expect(same).toMatchObject({ last: '200', last_side: 'sell', open_24h: '180', high_24h: '210', low_24h: '170', volume_24h: '500', trades_24h: 7, change_24h_bps: 1111 });
    const fill = stats({ last: '250', last_ts: NOW + 90_000, last_side: 'buy', open_24h: '250', high_24h: '250', low_24h: '240', volume_24h: '40', quote_volume_24h: '40', trades_24h: 2, change_24h_bps: 0, ts: NOW + 120_000 });
    expect(mergeStats(carried, fill, NOW + 120_000)).toMatchObject({ last: '250', last_side: 'buy', open_24h: '180', high_24h: '250', low_24h: '170', volume_24h: '540', quote_volume_24h: '540', trades_24h: 9, change_24h_bps: 3888 });
    // 24 h after the carried read its window is gone: only its last price remains
    const later = NOW + 25 * 3_600_000;
    expect(mergeStats(carried, stats({ ...noFigures, ts: later }), later)).toMatchObject({ last: '200', open_24h: null, high_24h: null, volume_24h: '0', trades_24h: 0, change_24h_bps: null });
  });

  it('tokens: the indexer facts stay, the order counts are the node book ones; a token only the node saw is added', () => {
    const carried = [tokenRow(TOKEN, { ticker: 'TUSD', decimals: 6, scale: 1_000_000 }), tokenRow(OTHER, { template_hash: null })];
    const seen = [tokenRow(OTHER, { template_hash: 'ef'.repeat(32), open_asks: 1, open_bids: 0, market_scale: null }), tokenRow('73'.repeat(32), { open_asks: 4 })];
    const out = mergeTokens(carried, seen);
    expect(out.map((r) => r.covenant_id)).toEqual([TOKEN, OTHER, '73'.repeat(32)]);
    expect(out[0]).toMatchObject({ ticker: 'TUSD', decimals: 6, scale: 1_000_000, open_asks: 0, open_bids: 0 });
    expect(out[1]).toMatchObject({ template_hash: 'ef'.repeat(32), open_asks: 1, market_scale: 1_000_000 });
    expect(out[2]).toMatchObject({ open_asks: 4 });
  });
});
