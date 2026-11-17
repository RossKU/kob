// The chart's mid line: (best bid + best ask) / 2 of the KAS book, from `GET /v1/quotes/{token}` (crates/kobd/src/indexer/quotes.rs). The indexer
// lists a bucket only when the touch changed, so the value in force is carried to each bar. A bar where a side was empty has no mid.
import type { CandleInterval, QuotesView } from '../../data/indexer-types';
import type { IndexerApi } from '../../data/indexer';
import { ratToNumber } from '../../kob/pair';
import { basisToNumber, INTERVAL_MS, priceText } from './market-model';
import { ratioText, tokensPerKas } from './orientation';

export type QuoteItem = QuotesView['items'][number];

const big = (s: string | null): bigint | null => (s !== null && /^\d+$/.test(s) ? BigInt(s) : null);

/** The mid of one item (sompi per basis, rounded down like the stats' `mid`), null when a side is empty. */
export function midOf(item: QuoteItem): bigint | null {
  const bid = big(item[1]);
  const ask = big(item[2]);
  return bid !== null && ask !== null ? (bid + ask) / 2n : null;
}

/** The mid in force at the end of each bar (`barTimes` in UTC seconds): the last item before the bar's end. Null before the first item. */
export function midOnBars(items: readonly QuoteItem[], barTimes: readonly number[], intervalMs: number): (bigint | null)[] {
  const out: (bigint | null)[] = [];
  let j = -1;
  for (const time of barTimes) {
    const end = time * 1000 + intervalMs;
    while (j + 1 < items.length && items[j + 1]![0] < end) j++;
    out.push(j >= 0 ? midOf(items[j]!) : null);
  }
  return out;
}

/** The chart's points (a bar with no mid is a gap) and the legend's text per bar; `inverted` draws 1/price as the candles do. */
export function midSeries(mids: readonly (bigint | null)[], barTimes: readonly number[], decimals: number, basis: bigint, dp: number, inverted: boolean) {
  const conv = (m: bigint): { n: number; t: string } | null => {
    if (!inverted) return { n: basisToNumber(m, decimals, basis), t: priceText(m, decimals, basis, dp) };
    const r = tokensPerKas(m, decimals, basis);
    return r ? { n: ratToNumber(r), t: ratioText(r, dp) } : null;
  };
  const vals = mids.map((m) => (m !== null && m > 0n ? conv(m) : null));
  const points = barTimes.map((time, i) => (vals[i] ? { time, value: vals[i]!.n } : { time }));
  return { points, hasData: vals.some((v) => v !== null), text: (i: number): string | null => vals[i]?.t ?? null };
}

/** The cached items before `from`, then the newer ones (an incremental load from the newest bucket on hand, inclusive); none newer: the cached ones. */
export function mergeQuotes(old: readonly QuoteItem[], fresh: readonly QuoteItem[], from: number, max: number): QuoteItem[] {
  if (!fresh.some((x) => x[0] >= from)) return [...old];
  const out = [...old.filter((x) => x[0] < from), ...fresh.filter((x) => x[0] >= from)];
  return out.length > max ? out.slice(out.length - max) : out;
}

/**
 * The quotes of the chart's window (`max` bars up to now), or with `prev` only those since its newest bucket, merged. Null on an indexer
 * without the route; a failed read keeps `prev`, so it never fails the candles.
 */
export async function loadQuotes(indexer: IndexerApi, token: string, interval: CandleInterval, prev: QuotesView | null, signal: AbortSignal, max: number, now = Date.now()): Promise<QuotesView | null> {
  if (!indexer.quotes) return null;
  const o = { signal, priority: 'low' as const };
  try {
    const last = prev?.items.at(-1);
    if (prev && last) {
      const next = await indexer.quotes(token, { interval, from: last[0], limit: max }, o);
      if (next && next.price_basis === prev.price_basis && next.items.length < max) return { ...next, items: mergeQuotes(prev.items, next.items, last[0], max) };
    }
    const iv = INTERVAL_MS[interval];
    return await indexer.quotes(token, { interval, from: Math.floor(now / iv) * iv - (max - 1) * iv, limit: max }, o);
  } catch (e) {
    if (signal.aborted) throw e;
    return prev;
  }
}
