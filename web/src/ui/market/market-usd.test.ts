import { describe, expect, it } from 'vitest';
import { rateOf, type Fiat } from '../../app/fiat';
import type { StatsView } from '../../data/indexer-types';
import type { TokenInfo } from '../../kob/registry';
import { marketCapCell, tokenEntry, usdCell, type Slot } from './market-list';
import { statsModel } from './market-model';
import { usdStatsModel } from './stats-usd';
import type { TokenRow } from './token-model';

const ID = (c: string) => c.repeat(64);
const fiat: Fiat = { rate: rateOf(0.0298)!, quote: { usd: 0.0298, at: 1_800_000_000, source: 'coingecko' }, testnet: false };
const RATE = 'USD estimate at $0.0298 per KAS';

function row(ticker: string, id: string, over: Partial<TokenRow> = {}): TokenRow {
  return {
    covenantId: id, ticker, name: `${ticker} coin`, source: 'registry', info: { covenantId: id, ticker } as unknown as TokenInfo, index: null, powers: [],
    issuerControl: false, facts: [], templateId: 'kcc20-ref-8x8', badges: [], tradable: true, reason: null, indexerProblems: [], openAsks: 3, openBids: 4, decimals: 8,
    scale: 100_000_000n, tick: 10_000n, ...over,
  };
}
function stats(token: string, over: Partial<StatsView> = {}): StatsView {
  return {
    token, price_basis: '100000000', decimals: 8, ts: 1_700_000_000_000, last: '2510000', last_ts: 1, last_side: 'buy', open_24h: '2500000', high_24h: '2600000',
    low_24h: '2400000', change_24h_bps: 40, volume_24h: '1500000000', quote_volume_24h: '37650000', trades_24h: 7, best_bid: '2500000', best_ask: '2520000',
    mid: '2510000', spread_bps: 79, open_asks: 3, open_bids: 4, depth_2pct_quote: '120000000000', ...over,
  };
}
const ok = <T,>(value: T): Slot<T> => ({ state: 'ok', value });

describe('the market list in USD', () => {
  it('shows the price, the 24 h volume, the market cap and the depth in dollars, each with its KAS figure and the rate in the tooltip; the change and the spread stay', () => {
    const r = row('EXKCC', ID('a'));
    const kas = tokenEntry(r, ok(stats(r.covenantId)));
    const usd = tokenEntry(r, ok(stats(r.covenantId)), fiat);
    // 0.0251 KAS x $0.0298
    expect(usd.cells.last).toEqual({ text: '$0.000748', title: `0.0251 (KAS per EXKCC) · ${RATE}` });
    // 0.3765 KAS
    expect(usd.cells.volume).toEqual({ text: '$0.01122', title: `0.3765 KAS · 7 trades in 24 h · ${RATE}` });
    // 1,200 KAS
    expect(usd.cells.depth).toEqual({ text: '$35.76', title: `1,200.00 KAS · 3 asks · 4 bids · ${RATE}` });
    expect(usd.cells.change).toEqual(kas.cells.change);
    expect(usd.cells.spread).toEqual(kas.cells.spread);
    // the sort is by the KAS value (the same order as the dollars)
    expect(usd.sort).toEqual(kas.sort);
  });

  it('KAS is the default: no fiat, no change', () => {
    const r = row('EXKCC', ID('a'));
    expect(tokenEntry(r, ok(stats(r.covenantId)), null)).toEqual(tokenEntry(r, ok(stats(r.covenantId))));
  });

  it('a market cap is the KAS cap in dollars; a cell that is too wide goes compact and its tooltip leads with the exact dollars', () => {
    // 56,000,000 supply x 0.0251 KAS = 1,405,600 KAS
    const supply = { units: 5_600_000_000_000_000n, source: 'registry' as const };
    const cap = marketCapCell({ num: 251n, den: 10_000n }, supply, 8, 'EXKCC');
    expect(cap.sompi).toBe(140_560_000_000_000n);
    const cell = usdCell(cap.cell, { num: cap.sompi! * fiat.rate.num, den: 100_000_000n * fiat.rate.den }, fiat);
    // 1,405,600 KAS x 0.0298 = $41,886.88
    expect(cell.text).toBe('$41,886.88');
    const big = usdCell({ text: '1', title: '1 KAS' }, { num: 123_456_789n * 100n + 25n, den: 100n }, fiat);
    expect(big.text).toBe('$123.46M');
    expect(big.title).toBe(`$123,456,789.25 · 1 KAS · ${RATE}`);
    // no figure, no dollars
    expect(usdCell({ text: null, title: null }, { num: 1n, den: 1n }, fiat)).toEqual({ text: null, title: null });
    expect(usdCell({ text: '1', title: null }, null, fiat)).toEqual({ text: '1', title: null });
  });

  it('a market without a price shows no dollars either', () => {
    const r = row('EXKCC', ID('a'));
    const e = tokenEntry(r, ok(stats(r.covenantId, { last: null, mid: null, quote_volume_24h: null, depth_2pct_quote: null } as Partial<StatsView>)), fiat);
    expect(e.cells.last.text).toBeNull();
    expect(e.cells.volume.text).toBeNull();
    expect(e.cells.depth.text).toBeNull();
  });
});

describe('the market page strip in USD', () => {
  const v = stats(ID('a'));
  const base = statsModel(v, 8, 4);

  it('the last price, the high, the low and the KAS volume become dollars; the change, the token volume and the trades stay', () => {
    const u = usdStatsModel(base, v, 8, fiat, 'EXKCC');
    expect(u.model.last).toBe('$0.000748');
    expect(u.model.high).toBe('$0.0007748');
    expect(u.model.low).toBe('$0.0007152');
    expect(u.model.quoteVolume).toBe('$0.01122');
    expect(u.model.quoteVolumeExact).toBe('$0.01122');
    expect(u.model.change).toBe(base.change);
    expect(u.model.volume).toBe(base.volume);
    expect(u.model.trades).toBe(base.trades);
    expect(u.model.lastSide).toBe(base.lastSide);
    // each converted tile keeps its KAS figure
    expect(u.lastTitle).toBe(`${base.last} KAS per EXKCC · ${RATE}`);
    expect(u.titles.high).toBe(`${base.high} KAS per EXKCC · ${RATE}`);
    expect(u.titles.low).toBe(`${base.low} KAS per EXKCC · ${RATE}`);
    expect(u.titles.quoteVolume).toBe(`${base.quoteVolumeExact} KAS · ${RATE}`);
  });

  it('without a last price in the stats the tape\'s last trade is converted; with none the tile stays empty; no stats at all converts nothing', () => {
    const noLast = { ...v, last: null } as unknown as StatsView;
    const b = statsModel(noLast, 8, 4);
    expect(usdStatsModel(b, noLast, 8, fiat, 'EXKCC', { price: 2_510_000n, basis: 100_000_000n }).model.last).toBe('$0.000748');
    expect(usdStatsModel(b, noLast, 8, fiat, 'EXKCC').model.last).toBeNull();
    const none = usdStatsModel(statsModel(null, 8, 4), null, 8, fiat, 'EXKCC');
    expect([none.model.last, none.model.high, none.model.low, none.model.quoteVolume]).toEqual([null, null, null, null]);
  });

  it('a large KAS volume goes compact in dollars', () => {
    const w = stats(ID('a'), { quote_volume_24h: '500000000000000' });
    const u = usdStatsModel(statsModel(w, 8, 4), w, 8, fiat, 'EXKCC');
    // 5,000,000 KAS x 0.0298 = $149,000
    expect(u.model.quoteVolume).toBe('$149,000.00');
    const x = stats(ID('a'), { quote_volume_24h: '50000000000000000' });
    // 500,000,000 KAS = $14.90M
    expect(usdStatsModel(statsModel(x, 8, 4), x, 8, fiat, 'EXKCC').model.quoteVolume).toBe('$14.90M');
  });
});
