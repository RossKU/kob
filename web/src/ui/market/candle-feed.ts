// The chart's candle loads: the full series (MAX_BARS bars) on the first view and on a change of token, interval or orientation source, and after
// that only the bars since the newest one on screen (`from` = its bucket start, inclusive: that bar may still be growing), merged into the cached
// series. A live refresh then reads a handful of buckets instead of 1500 (the indexer scans only the window from `from`). Anything that could have
// rewritten older bars (a reorg, a resync, a new price basis, an answer as long as the limit) takes the full load again.
import type { CandleInterval, CandlesView, PairCandlesView } from '../../data/indexer-types';
import type { IndexerApi } from '../../data/indexer';

export const MAX_BARS = 1500;

/** What one chart draws: a token's KAS candles, the indexer's pair candles, or the two KAS legs of a client-side pair ratio. */
export type ChartView =
  | { kind: 'kas'; v: CandlesView }
  | { kind: 'pair'; pc: PairCandlesView }
  | { kind: 'usd'; q: CandlesView; b: CandlesView };

/** The chart's source: one token's KAS book, or a pair BASE/QUOTE (`pairCandles`: ask the indexer for its pair candles first). */
export type ChartSource = { token: string } | { base: string; quote: string; pairCandles?: boolean };

/** The bars of `old` before `from`, then `fresh` (ascending; a bucket in both is taken from `fresh`), the newest `max` of them. */
export function mergeBars<T extends { t: number }>(old: readonly T[], fresh: readonly T[], from: number, max: number = MAX_BARS): T[] {
  const kept = old.filter((x) => x.t < from);
  const added = [...fresh].filter((x) => x.t >= from).sort((a, b) => a.t - b.t);
  const out = [...kept, ...added];
  return out.length > max ? out.slice(out.length - max) : out;
}

const lastT = (items: readonly { t: number }[]): number | null => (items.length ? items[items.length - 1]!.t : null);

/** The bucket start an incremental load reads from (the newest bar on screen; for two legs the older of their newest bars), null: none, load in full. */
export function incrementalFrom(view: ChartView): number | null {
  if (view.kind === 'kas') return lastT(view.v.items);
  if (view.kind === 'pair') return lastT(view.pc.items);
  const q = lastT(view.q.items);
  const b = lastT(view.b.items);
  return q === null || b === null ? null : Math.min(q, b);
}

/** One KAS series merged with its newer bars, or null when only a full load is safe (unsupported now, another basis, an answer cut at the limit). */
function mergeKas(prev: CandlesView, next: CandlesView | null, from: number, limit: number): CandlesView | null {
  if (!next || next.price_basis !== prev.price_basis || next.items.length >= limit) return null;
  return { ...next, items: mergeBars(prev.items, next.items, from, limit) };
}

async function loadFull(indexer: IndexerApi, src: ChartSource, interval: CandleInterval, signal: AbortSignal, limit: number): Promise<ChartView | null> {
  if ('token' in src) {
    const v = await indexer.candles(src.token, { interval, limit }, { signal });
    return v ? { kind: 'kas', v } : null;
  }
  if (src.pairCandles) {
    const pc = await indexer.pairCandles(src.base, src.quote, { interval, limit }, { signal });
    if (pc) return { kind: 'pair', pc };
  }
  // both legs on the same interval and window
  const [q, b] = await Promise.all([indexer.candles(src.quote, { interval, limit }, { signal }), indexer.candles(src.base, { interval, limit }, { signal })]);
  return q && b ? { kind: 'usd', q, b } : null;
}

async function loadNewer(indexer: IndexerApi, src: ChartSource, prev: ChartView, from: number, interval: CandleInterval, signal: AbortSignal, limit: number): Promise<ChartView | null> {
  const q = { interval, from, limit };
  if (prev.kind === 'kas' && 'token' in src) {
    const v = mergeKas(prev.v, await indexer.candles(src.token, q, { signal }), from, limit);
    return v ? { kind: 'kas', v } : null;
  }
  if ('token' in src) return null;
  if (prev.kind === 'pair') {
    const pc = await indexer.pairCandles(src.base, src.quote, q, { signal });
    if (!pc || pc.price_basis !== prev.pc.price_basis || pc.quote_price_basis !== prev.pc.quote_price_basis || pc.items.length >= limit) return null;
    return { kind: 'pair', pc: { ...pc, items: mergeBars(prev.pc.items, pc.items, from, limit) } };
  }
  if (prev.kind === 'usd') {
    const [nq, nb] = await Promise.all([indexer.candles(src.quote, q, { signal }), indexer.candles(src.base, q, { signal })]);
    const mq = mergeKas(prev.q, nq, from, limit);
    const mb = mergeKas(prev.b, nb, from, limit);
    return mq && mb ? { kind: 'usd', q: mq, b: mb } : null;
  }
  return null;
}

/**
 * The series to draw: with `prev` (the cached series of the same source and interval, still valid) only the bars since its newest one, merged into
 * it; else, or when the newer bars cannot be merged safely, the full series.
 */
export async function loadCandles(
  indexer: IndexerApi,
  src: ChartSource,
  interval: CandleInterval,
  prev: ChartView | null,
  signal: AbortSignal,
  limit: number = MAX_BARS,
): Promise<ChartView | null> {
  const from = prev ? incrementalFrom(prev) : null;
  if (prev && from !== null) {
    const merged = await loadNewer(indexer, src, prev, from, interval, signal, limit);
    if (merged) return merged;
  }
  return loadFull(indexer, src, interval, signal, limit);
}
