import { describe, expect, it } from 'vitest';
import type { ActivityStatsView, CandleView, IndexerTokenView, PairSummaryView } from '../../data/indexer-types';
import { activityFigures, activitySince, axisTop, barPct, booksNow, changePct, compactKas, fineKas, summarizeAllTime, summarizeWindow, WINDOWS, windowEnd } from './summary-model';

const H = 3_600_000;
const D = 24 * H;
const KAS = 100_000_000n;
const c = (t: number, kas: number, trades: number): CandleView => ({ t, o: '1', h: '1', l: '1', c: '1', volume: '1', quote_volume: (BigInt(kas) * KAS).toString(), trades });

describe('the window', () => {
  // 2026-11-08 13:20 UTC
  const now = Date.UTC(2026, 10, 8, 13, 20);

  it('ends with the bar that holds now, aligned to its length', () => {
    expect(windowEnd(now, WINDOWS['1D'])).toBe(Date.UTC(2026, 10, 8, 14));
    expect(windowEnd(now, WINDOWS['7D'])).toBe(Date.UTC(2026, 10, 8, 18));
    expect(windowEnd(now, WINDOWS['1M'])).toBe(Date.UTC(2026, 10, 9));
  });

  it('sums every token into its bars, and the window before into the comparison', () => {
    const end = windowEnd(now, WINDOWS['1D']);
    const a = [c(end - 25 * H, 7, 1), c(end - 24 * H, 10, 2), c(end - H, 5, 1)];
    const b = [c(end - H, 3, 4)];
    const idle = [c(end - 30 * H, 9, 1)];
    const s = summarizeWindow('1D', [a, b, idle], now);
    expect(s.bars).toHaveLength(24);
    expect(s.bars[0]).toMatchObject({ t: end - 24 * H, quote: 10n * KAS, trades: 2 });
    expect(s.bars[23]).toMatchObject({ quote: 8n * KAS, trades: 5 });
    expect([s.quote, s.trades, s.activeTokens]).toEqual([18n * KAS, 7, 2]);
    expect([s.prevQuote, s.prevTrades]).toEqual([16n * KAS, 2]);
  });

  it('has no comparison when nothing is known before the window', () => {
    const end = windowEnd(now, WINDOWS['1D']);
    const s = summarizeWindow('1D', [[c(end - H, 5, 1)]], now);
    expect([s.prevQuote, s.prevTrades]).toEqual([null, null]);
    expect(changePct(s.quote, s.prevQuote)).toBeNull();
  });

  it('puts 6 hours in a 7D bar and a week in a 1Y bar', () => {
    const end7 = windowEnd(now, WINDOWS['7D']);
    const s7 = summarizeWindow('7D', [[c(end7 - 6 * H, 1, 1), c(end7 - H, 2, 1)]], now);
    expect(s7.bars).toHaveLength(28);
    expect(s7.bars[27]!.quote).toBe(3n * KAS);
    const endY = windowEnd(now, WINDOWS['1Y']);
    const sY = summarizeWindow('1Y', [[c(endY - 7 * D, 1, 1), c(endY - D, 2, 1), c(endY - 8 * D, 4, 1)]], now);
    expect(sY.bars).toHaveLength(52);
    expect([sY.bars[51]!.quote, sY.bars[50]!.quote]).toEqual([3n * KAS, 4n * KAS]);
  });
});

describe('all time and the books now', () => {
  it('sums every daily candle; since is the first day with a trade', () => {
    const a = summarizeAllTime([[c(5 * D, 0, 0), c(6 * D, 2, 3)], [c(9 * D, 1, 1)]]);
    expect(a).toEqual({ quote: 3n * KAS, trades: 4, since: 6 * D });
    expect(summarizeAllTime([])).toEqual({ quote: 0n, trades: 0, since: null });
  });

  it('counts the open orders of the tokens and the pairs', () => {
    const tok = (asks: number, bids: number) => ({ open_asks: asks, open_bids: bids }) as IndexerTokenView;
    const pair: PairSummaryView = { base: 'aa', quote: 'bb', direct_asks: 1, direct_bids: 2, entry_asks: 0, entry_bids: 1, conditionals: 3 };
    expect(booksNow([tok(2, 1), tok(0, 0), tok(0, 4)], [pair])).toEqual({ asks: 2, bids: 5, pairOrders: 7, tokensWithOrders: 2, pairs: 1 });
  });

  it('counts a pair once whichever way its orders are placed, and every order of both', () => {
    const p = (base: string, quote: string): PairSummaryView => ({ base, quote, direct_asks: 1, direct_bids: 0, entry_asks: 0, entry_bids: 0, conditionals: 1 });
    const got = booksNow([], [p('aa', 'bb'), p('bb', 'aa'), p('aa', 'cc')]);
    expect([got.pairs, got.pairOrders]).toEqual([2, 6]);
  });
});

describe('numbers', () => {
  it('compacts KAS without rounding up', () => {
    expect([compactKas(9_840n * KAS), compactKas(412_999n * KAS), compactKas(9_819_999n * KAS), compactKas(0n)]).toEqual(['9,840', '412k', '9.81M', '0']);
  });

  it('changes in percent, one decimal', () => {
    expect([changePct(118, 100), changePct(50n, 100n), changePct(0, 0), changePct(5, 0)]).toEqual([18, -50, 0, null]);
  });

  it('tops the axis at 1, 2 or 5 times a power of ten', () => {
    const bars = (q: bigint[]) => q.map((quote) => ({ t: 0, quote, trades: 1 }));
    expect([axisTop(bars([7n, 3n])), axisTop(bars([12n])), axisTop(bars([340n])), axisTop(bars([0n]))]).toEqual([10n, 20n, 500n, 0n]);
    expect([barPct(25n, 100n), barPct(1n, 0n)]).toEqual(['25.0%', '0%']);
  });
});

describe('the activity totals', () => {
  const view: ActivityStatsView = { since: null, ts: 1, orders: 42, traders: 9, executors: null, fees_paid: '250000000', kas_locked: '880000000000' };

  it('reads them as numbers and sompi; every figure is a dash for an indexer without them', () => {
    expect(activityFigures(view)).toEqual({ orders: 42, traders: 9, executors: null, paid: 250_000_000n, locked: 880_000_000_000n });
    expect(activityFigures({ ...view, fees_paid: null, traders: null })).toMatchObject({ traders: null, paid: null });
    const none = { orders: null, traders: null, executors: null, paid: null, locked: null };
    expect([activityFigures(null), activityFigures(undefined)]).toEqual([none, none]);
  });

  it('shows the executors an indexer counts, a dash where it counts none', () => {
    expect(activityFigures({ ...view, executors: 3 }).executors).toBe(3);
    expect(activityFigures({ ...view, executors: 0 }).executors).toBe(0);
    expect(activityFigures(view).executors).toBeNull();
  });

  it('starts a window as far back as the page words it, at a whole minute', () => {
    const now = Date.UTC(2026, 10, 8, 13, 20, 41, 500);
    expect(activitySince('1D', now)).toBe(Date.UTC(2026, 10, 7, 13, 20));
    expect(activitySince('7D', now)).toBe(Date.UTC(2026, 10, 1, 13, 20));
    expect(activitySince('1M', now)).toBe(Date.UTC(2026, 9, 9, 13, 20));
    expect(activitySince('1Y', now)).toBe(now - 364 * D - (now % 60_000));
  });

  it('shows a small sum to the cent and a large one compact, never rounded up', () => {
    expect([fineKas(0n), fineKas(37_999_999n), fineKas(1_250_000_000n), fineKas(84_000n * KAS), fineKas(412_999n * KAS)]).toEqual(['0', '0.37', '12.5', '84,000', '412k']);
    expect(fineKas(999n * KAS + 99_999_999n)).toBe('999.99');
  });
});
