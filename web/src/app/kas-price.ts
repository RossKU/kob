// The KAS/USD price feed of the optional USD display: one store for the whole page and the loop that fills it.
//
// The loop runs only while USD is the display currency (app/currency-pref.ts), asks the indexer (`kasUsd`, data/kas-price.ts) once at the start and then every
// KAS_USD_POLL_MS (a minute) while the page is shown, and once more when the page is shown again after a poll was due. An indexer that has no such route (null:
// 404) is "unavailable" and is asked again only after KAS_USD_RETRY_UNSUPPORTED_MS; a failed read (a 503 before the indexer's first price, a network error) keeps the
// last quote (its age decides whether it is still used, app/fiat.ts) and doubles the wait up to KAS_USD_MAX_BACKOFF_MS, which the first success resets. Nothing here is
// authority for anything signed.
import { KAS_USD_MAX_BACKOFF_MS, KAS_USD_POLL_MS, KAS_USD_RETRY_UNSUPPORTED_MS, type KasUsdQuote } from '../data/kas-price';
import type { IndexerApi } from '../data/indexer';
import type { PriceFeed } from './fiat';

/** What the store holds: the feed's state and the clock the views judge a quote's age by (it moves while the loop runs). */
export interface PriceSnapshot extends PriceFeed {
  nowMs: number;
}

/** How often the views re-judge the age of the quote, ms (the loop's own tick). */
export const PRICE_TICK_MS = 30_000;

export class KasPriceStore {
  private snap: PriceSnapshot = { quote: null, seenAtMs: null, status: 'idle', nowMs: Date.now() };
  private readonly listeners = new Set<() => void>();

  get snapshot(): PriceSnapshot {
    return this.snap;
  }

  subscribe(l: () => void): () => void {
    this.listeners.add(l);
    return () => void this.listeners.delete(l);
  }

  update(f: (s: PriceSnapshot) => PriceSnapshot): void {
    const next = f(this.snap);
    if (next === this.snap) return;
    this.snap = next;
    for (const l of [...this.listeners]) l();
  }

  /** A quote came in: kept with the local time its `at` was first seen (a quote with the same `at` keeps its first time). */
  received(q: KasUsdQuote, nowMs: number): void {
    this.update((s) => ({ quote: q, seenAtMs: s.quote && s.quote.at === q.at && s.quote.usd === q.usd ? s.seenAtMs : nowMs, status: 'ok', nowMs }));
  }

  /** Back to the start (tests, and a new indexer). */
  reset(nowMs = Date.now()): void {
    this.update(() => ({ quote: null, seenAtMs: null, status: 'idle', nowMs }));
  }
}

/** The page's one store. */
export const kasPrice = new KasPriceStore();

export interface FeedDeps {
  /** null: no indexer (the price is unavailable) */
  indexer: Pick<IndexerApi, 'kasUsd'> | null;
  store?: KasPriceStore;
  pollMs?: number;
  retryUnsupportedMs?: number;
  maxBackoffMs?: number;
  tickMs?: number;
  now?: () => number;
  visible?: () => boolean;
  /** calls `cb` on every change of the page's visibility; returns the unsubscribe */
  onVisibility?: (cb: (visible: boolean) => void) => () => void;
  setTimeout?: (cb: () => void, ms: number) => unknown;
  clearTimeout?: (h: unknown) => void;
}

/**
 * Starts the loop; returns the stop. Reads once at once, then every `pollMs` while the page is visible (a hidden page sleeps; shown again, it reads at once when the
 * last read is a poll old). One read at a time.
 */
export function startKasPriceFeed(d: FeedDeps): () => void {
  const store = d.store ?? kasPrice;
  const pollMs = d.pollMs ?? KAS_USD_POLL_MS;
  const retryMs = d.retryUnsupportedMs ?? KAS_USD_RETRY_UNSUPPORTED_MS;
  const maxBackoffMs = d.maxBackoffMs ?? KAS_USD_MAX_BACKOFF_MS;
  const tickMs = d.tickMs ?? PRICE_TICK_MS;
  const now = d.now ?? Date.now;
  const visible = d.visible ?? (() => typeof document === 'undefined' || document.visibilityState !== 'hidden');
  const later = d.setTimeout ?? ((cb: () => void, ms: number) => setTimeout(cb, ms));
  const cancel = d.clearTimeout ?? ((h: unknown) => clearTimeout(h as ReturnType<typeof setTimeout>));
  let stopped = false;
  let busy = false;
  // failed reads in a row: each doubles the wait, a success resets it
  let fails = 0;
  let timer: unknown = null;
  let ticker: unknown = null;
  // the earliest the next read may happen (ms on `now`); a hidden page leaves it due
  let nextAt = 0;
  const ctl = typeof AbortController !== 'undefined' ? new AbortController() : null;

  const schedule = (): void => {
    if (timer !== null) cancel(timer);
    timer = null;
    if (stopped || !visible()) return;
    timer = later(() => void read(), Math.max(0, nextAt - now()));
  };

  const read = async (): Promise<void> => {
    if (timer !== null) cancel(timer);
    timer = null;
    if (stopped || busy || !visible()) return;
    busy = true;
    store.update((s) => (s.quote ? s : s.status === 'loading' ? s : { ...s, status: 'loading', nowMs: now() }));
    let wait = pollMs;
    try {
      const fn = d.indexer?.kasUsd;
      const q = fn && d.indexer ? await fn.call(d.indexer, { priority: 'low', ...(ctl ? { signal: ctl.signal } : {}) }) : null;
      if (stopped) return;
      if (q) {
        fails = 0;
        store.received(q, now());
      } else {
        fails = 0;
        // no such route here (another indexer after a failover, or one with the price off): an older quote is dropped, it would only age
        store.update((s) => ({ ...s, quote: null, seenAtMs: null, status: 'unavailable', nowMs: now() }));
        wait = retryMs;
      }
    } catch {
      if (stopped) return;
      // the last quote is kept: its age decides whether it is still used
      store.update((s) => ({ ...s, status: 'error', nowMs: now() }));
      fails += 1;
      wait = Math.min(pollMs * 2 ** fails, Math.max(maxBackoffMs, pollMs));
    } finally {
      busy = false;
    }
    nextAt = now() + wait;
    schedule();
  };

  const off = (d.onVisibility ?? (() => () => {}))((v) => {
    if (stopped) return;
    if (v) {
      // shown again: a read that fell due while hidden is made now
      if (now() >= nextAt) void read();
      else schedule();
    } else if (timer !== null) {
      cancel(timer);
      timer = null;
    }
  });
  const tick = (): void => {
    if (stopped) return;
    store.update((s) => ({ ...s, nowMs: now() }));
    ticker = later(tick, tickMs);
  };
  ticker = later(tick, tickMs);
  void read();
  return () => {
    stopped = true;
    if (timer !== null) cancel(timer);
    if (ticker !== null) cancel(ticker);
    ctl?.abort();
    off();
  };
}
