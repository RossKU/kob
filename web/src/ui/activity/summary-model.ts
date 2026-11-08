// The Activity page's summary (ActivitySummary.tsx): all time, a time window (1D / 7D / 1M / 1Y) with its volume chart, and the books now. Pure, tested
// in summary-model.test.ts. Built from what the indexer serves today: the KAS candles of every token (`/v1/candles`), the token list's open orders
// and the pairs' live orders. Volume and trades are the KAS markets' (exact, sompi): a pair fill carries no KAS price, and most go through the KAS
// books, whose fills these already count. What the indexer does not report yet (orders placed, traders, executors, fees paid to them, the KAS locked
// in orders) is null: the page shows "—".
import type { CandleView, IndexerTokenView, PairSummaryView } from '../../data/indexer-types';

export type SummaryWindow = '1D' | '7D' | '1M' | '1Y';
export const SUMMARY_WINDOWS: readonly SummaryWindow[] = ['1D', '7D', '1M', '1Y'];

const HOUR = 3_600_000;
const DAY = 24 * HOUR;

export interface WindowDef {
  /** the candle interval the window is read from */
  interval: '1h' | '1d';
  /** one bar */
  bucketMs: number;
  /** bars in the window */
  buckets: number;
  /** dictionary key of one bar's length (`activity.summary.unit.*`) */
  unit: 'hour' | 'sixHours' | 'day' | 'week';
}

export const WINDOWS: Record<SummaryWindow, WindowDef> = {
  '1D': { interval: '1h', bucketMs: HOUR, buckets: 24, unit: 'hour' },
  '7D': { interval: '1h', bucketMs: 6 * HOUR, buckets: 28, unit: 'sixHours' },
  '1M': { interval: '1d', bucketMs: DAY, buckets: 30, unit: 'day' },
  '1Y': { interval: '1d', bucketMs: 7 * DAY, buckets: 52, unit: 'week' },
};

/** Hourly candles a token needs for 1D and 7D with their previous windows (two weeks), and daily ones for everything (all time). */
export const HOURLY_LIMIT = 14 * 24;
export const DAILY_LIMIT = 1500;

export interface Bar {
  /** bar start, unix ms */
  t: number;
  /** KAS markets' volume, sompi */
  quote: bigint;
  trades: number;
}

export interface WindowSummary {
  window: SummaryWindow;
  bars: Bar[];
  quote: bigint;
  trades: number;
  /** the same totals over the window before (null when nothing is known that far back) */
  prevQuote: bigint | null;
  prevTrades: number | null;
  /** tokens with a KAS trade in the window */
  activeTokens: number;
}

/** The end of the window: the end of the bar that holds `now` (bars are aligned to UTC multiples of their length). */
export function windowEnd(now: number, def: WindowDef): number {
  return Math.floor(now / def.bucketMs) * def.bucketMs + def.bucketMs;
}

/** One window's bars and totals over the candles of every token (`series`: one ascending list of `def.interval` candles per token). */
export function summarizeWindow(window: SummaryWindow, series: readonly (readonly CandleView[])[], now: number): WindowSummary {
  const def = WINDOWS[window];
  const end = windowEnd(now, def);
  const from = end - def.buckets * def.bucketMs;
  const prevFrom = from - def.buckets * def.bucketMs;
  const bars: Bar[] = Array.from({ length: def.buckets }, (_, i) => ({ t: from + i * def.bucketMs, quote: 0n, trades: 0 }));
  let prevQuote = 0n;
  let prevTrades = 0;
  let seenPrev = false;
  let active = 0;
  for (const candles of series) {
    let traded = false;
    for (const c of candles) {
      if (c.t >= from && c.t < end) {
        const b = bars[Math.floor((c.t - from) / def.bucketMs)]!;
        b.quote += big(c.quote_volume);
        b.trades += c.trades;
        if (c.trades > 0) traded = true;
      } else if (c.t >= prevFrom && c.t < from) {
        prevQuote += big(c.quote_volume);
        prevTrades += c.trades;
        seenPrev = true;
      } else if (c.t < prevFrom) {
        seenPrev = true;
      }
    }
    if (traded) active++;
  }
  return {
    window,
    bars,
    quote: bars.reduce((s, b) => s + b.quote, 0n),
    trades: bars.reduce((s, b) => s + b.trades, 0),
    prevQuote: seenPrev ? prevQuote : null,
    prevTrades: seenPrev ? prevTrades : null,
    activeTokens: active,
  };
}

export interface AllTime {
  quote: bigint;
  trades: number;
  /** the first day with a trade, unix ms (null: no trade yet) */
  since: number | null;
}

/** All-time KAS volume and trades over every token's daily candles. */
export function summarizeAllTime(daily: readonly (readonly CandleView[])[]): AllTime {
  let quote = 0n;
  let trades = 0;
  let since: number | null = null;
  for (const candles of daily) {
    for (const c of candles) {
      quote += big(c.quote_volume);
      trades += c.trades;
      if (c.trades > 0 && (since === null || c.t < since)) since = c.t;
    }
  }
  return { quote, trades, since };
}

export interface BooksNow {
  bids: number;
  asks: number;
  pairOrders: number;
  tokensWithOrders: number;
  pairs: number;
}

/** The books now: the token list's listed open orders and the pairs' live pair orders. */
export function booksNow(tokens: readonly IndexerTokenView[], pairs: readonly PairSummaryView[]): BooksNow {
  let bids = 0;
  let asks = 0;
  let withOrders = 0;
  for (const t of tokens) {
    const a = t.open_asks ?? 0;
    const b = t.open_bids ?? 0;
    asks += a;
    bids += b;
    if (a + b > 0) withOrders++;
  }
  const pairOrders = pairs.reduce((s, p) => s + p.direct_asks + p.direct_bids + p.entry_asks + p.entry_bids + p.conditionals, 0);
  return { bids, asks, pairOrders, tokensWithOrders: withOrders, pairs: pairs.length };
}

/** Change against the window before, in whole percent (null: nothing to compare with). */
export function changePct(now: bigint | number, prev: bigint | number | null): number | null {
  if (prev === null) return null;
  const p = BigInt(prev);
  const n = BigInt(now);
  if (p === 0n) return n === 0n ? 0 : null;
  return Number(((n - p) * 1000n) / p) / 10;
}

/** Whole KAS of a sompi amount, compact: 9,840 · 412k · 9.81M (no rounding up past what was traded). */
export function compactKas(sompi: bigint): string {
  const kas = Number(sompi / 1_000_000n) / 100;
  if (kas >= 1e9) return `${trim(kas / 1e9)}B`;
  if (kas >= 1e6) return `${trim(kas / 1e6)}M`;
  if (kas >= 1e5) return `${Math.floor(kas / 1e3)}k`;
  return Math.floor(kas).toLocaleString('en-US');
}

/** The chart's top: the highest bar rounded up to 1, 2 or 5 times a power of ten (0 for an empty window). */
export function axisTop(bars: readonly Bar[]): bigint {
  let peak = 0n;
  for (const b of bars) if (b.quote > peak) peak = b.quote;
  if (peak === 0n) return 0n;
  let step = 1n;
  while (step * 10n <= peak) step *= 10n;
  for (const m of [1n, 2n, 5n, 10n]) if (m * step >= peak) return m * step;
  return 10n * step;
}

/** A bar's height in percent of `top`, one decimal (0 when the window is empty). */
export function barPct(quote: bigint, top: bigint): string {
  if (top === 0n) return '0%';
  return `${(Number((quote * 1000n) / top) / 10).toFixed(1)}%`;
}

function trim(v: number): string {
  return (Math.floor(v * 100) / 100).toFixed(2).replace(/\.?0+$/, '');
}

function big(s: string | null | undefined): bigint {
  if (!s || !/^\d+$/.test(s)) return 0n;
  return BigInt(s);
}
