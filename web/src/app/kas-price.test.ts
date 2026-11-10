import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { KasUsdQuote } from '../data/kas-price';
import { KasPriceStore, startKasPriceFeed, type FeedDeps } from './kas-price';

const Q = (usd: number, at: number): KasUsdQuote => ({ usd, at, source: 'coingecko' });

describe('the KAS price feed', () => {
  beforeEach(() => vi.useFakeTimers({ now: 1_800_000_000_000 }));
  afterEach(() => vi.useRealTimers());

  function setup(answers: (n: number) => Promise<KasUsdQuote | null> | KasUsdQuote | null, o: Partial<FeedDeps> = {}) {
    const store = new KasPriceStore();
    let calls = 0;
    const seen: unknown[] = [];
    let visible = true;
    let onVis: ((v: boolean) => void) | null = null;
    const indexer = {
      kasUsd: async (opts?: unknown) => {
        seen.push(opts);
        return answers(++calls);
      },
    };
    const stop = startKasPriceFeed({
      indexer, store, now: Date.now, visible: () => visible, onVisibility: (cb) => ((onVis = cb), () => (onVis = null)),
      ...o,
    });
    return {
      store, stop, seen, calls: () => calls,
      hide: () => { visible = false; onVis?.(false); },
      show: () => { visible = true; onVis?.(true); },
    };
  }

  it('reads once at the start, then every minute, and keeps each quote with the time it was first seen', async () => {
    const t = setup((n) => Q(0.03 + n / 1000, Date.now() / 1000));
    await vi.advanceTimersByTimeAsync(0);
    expect(t.calls()).toBe(1);
    expect(t.store.snapshot).toMatchObject({ status: 'ok', quote: { usd: 0.031 } });
    expect(t.seen[0]).toMatchObject({ priority: 'low' });
    await vi.advanceTimersByTimeAsync(59_000);
    expect(t.calls()).toBe(1);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(t.calls()).toBe(2);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(t.calls()).toBe(3);
    expect(t.store.snapshot.quote?.usd).toBe(0.033);
    t.stop();
    await vi.advanceTimersByTimeAsync(900_000);
    expect(t.calls()).toBe(3);
  });

  it('a quote with the same time keeps its first-seen time (so its age keeps growing here)', async () => {
    const at = Date.now() / 1000;
    const t = setup(() => Q(0.03, at));
    await vi.advanceTimersByTimeAsync(0);
    const first = t.store.snapshot.seenAtMs;
    await vi.advanceTimersByTimeAsync(60_000);
    expect(t.calls()).toBe(2);
    expect(t.store.snapshot.seenAtMs).toBe(first);
    t.stop();
  });

  it('sleeps while the page is hidden and reads at once when it is shown after a read fell due', async () => {
    const t = setup(() => Q(0.03, Date.now() / 1000));
    await vi.advanceTimersByTimeAsync(0);
    expect(t.calls()).toBe(1);
    t.hide();
    await vi.advanceTimersByTimeAsync(20 * 60_000);
    expect(t.calls()).toBe(1);
    t.show();
    await vi.advanceTimersByTimeAsync(0);
    expect(t.calls()).toBe(2);
    // shown again before the next one is due: it waits for its time
    t.hide();
    t.show();
    await vi.advanceTimersByTimeAsync(0);
    expect(t.calls()).toBe(2);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(t.calls()).toBe(3);
    t.stop();
  });

  it('does nothing while the page starts hidden, and reads when it is first shown', async () => {
    let visible = false;
    let onVis: ((v: boolean) => void) | null = null;
    let calls = 0;
    const store = new KasPriceStore();
    const stop = startKasPriceFeed({
      indexer: { kasUsd: async () => (calls++, Q(0.03, Date.now() / 1000)) }, store, visible: () => visible, onVisibility: (cb) => ((onVis = cb), () => undefined),
    });
    await vi.advanceTimersByTimeAsync(600_000);
    expect(calls).toBe(0);
    visible = true;
    onVis!(true);
    await vi.advanceTimersByTimeAsync(0);
    expect(calls).toBe(1);
    stop();
  });

  it('an indexer without the route is unavailable, any older quote is dropped, and it is asked again only much later', async () => {
    let n = 0;
    const t = setup(() => (++n === 1 ? Q(0.03, Date.now() / 1000) : null), { retryUnsupportedMs: 10 * 60_000 });
    await vi.advanceTimersByTimeAsync(0);
    expect(t.store.snapshot.quote).not.toBeNull();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(t.store.snapshot).toMatchObject({ status: 'unavailable', quote: null, seenAtMs: null });
    await vi.advanceTimersByTimeAsync(9 * 60_000);
    expect(t.calls()).toBe(2);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(t.calls()).toBe(3);
    t.stop();
  });

  it('no indexer, or one without the method, is unavailable at once', async () => {
    const store = new KasPriceStore();
    const stop = startKasPriceFeed({ indexer: null, store, visible: () => true });
    await vi.advanceTimersByTimeAsync(0);
    expect(store.snapshot.status).toBe('unavailable');
    stop();
    const store2 = new KasPriceStore();
    const stop2 = startKasPriceFeed({ indexer: {} as never, store: store2, visible: () => true });
    await vi.advanceTimersByTimeAsync(0);
    expect(store2.snapshot.status).toBe('unavailable');
    stop2();
  });

  it('a failed read keeps the last quote and the next read waits twice as long', async () => {
    let n = 0;
    const t = setup(() => {
      if (++n === 2) throw new Error('503');
      return Q(0.03, Date.now() / 1000);
    });
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(t.calls()).toBe(2);
    expect(t.store.snapshot).toMatchObject({ status: 'error', quote: { usd: 0.03 } });
    // not at the normal pace: twice as long
    await vi.advanceTimersByTimeAsync(60_000);
    expect(t.calls()).toBe(2);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(t.calls()).toBe(3);
    expect(t.store.snapshot.status).toBe('ok');
    t.stop();
  });

  it('failures in a row double the wait up to 10 minutes and a success brings back the minute', async () => {
    let n = 0;
    const fail = new Set([1, 2, 3, 4, 5, 6, 7]);
    const t = setup(() => {
      if (fail.has(++n)) throw new Error('429');
      return Q(0.03, Date.now() / 1000);
    });
    const waitOf = async (): Promise<number> => {
      // the minutes (in 10 s steps) until the next read after the one just made
      const calls = t.calls();
      let ms = 0;
      while (t.calls() === calls && ms < 20 * 60_000) {
        await vi.advanceTimersByTimeAsync(10_000);
        ms += 10_000;
      }
      return ms / 60_000;
    };
    await vi.advanceTimersByTimeAsync(0);
    expect(t.calls()).toBe(1);
    // read 1 failed: 2, then 4, 8, then the cap of 10 (twice), the 7th read fails too, the 8th is ok
    expect([await waitOf(), await waitOf(), await waitOf(), await waitOf(), await waitOf(), await waitOf(), await waitOf()]).toEqual([2, 4, 8, 10, 10, 10, 10]);
    expect(t.calls()).toBe(8);
    expect(t.store.snapshot.status).toBe('ok');
    // ok: the next read is a minute later
    expect(await waitOf()).toBe(1);
    t.stop();
  });

  it('no quote is kept from an error before the first one, and the status says so', async () => {
    const t = setup(() => {
      throw new Error('503');
    });
    await vi.advanceTimersByTimeAsync(0);
    expect(t.store.snapshot).toMatchObject({ status: 'error', quote: null });
    t.stop();
  });

  it('the clock the views judge a quote by moves while it runs', async () => {
    const t = setup(() => Q(0.03, Date.now() / 1000));
    await vi.advanceTimersByTimeAsync(0);
    const at = t.store.snapshot.nowMs;
    await vi.advanceTimersByTimeAsync(31_000);
    expect(t.store.snapshot.nowMs).toBeGreaterThanOrEqual(at + 30_000);
    t.stop();
  });
});
