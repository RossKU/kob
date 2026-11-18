// Small hooks shared by the views: abortable async loading with coalesced refreshes, polling, a
// ticking clock and page visibility.
import { useCallback, useEffect, useRef, useState } from 'preact/hooks';

export interface AsyncState<T> {
  data: T | undefined;
  error: Error | null;
  /**
   * A request is in flight. It is also true during a reload, while `data` still holds the previous
   * result.
   */
  loading: boolean;
  /** Re-runs the loader now, aborting a run in flight (a manual retry). */
  reload(): void;
  /**
   * Re-runs the loader WITHOUT aborting a run in flight. Calls made during a run fold into ONE run
   * right after it (`Coalescer`).
   */
  refresh(): void;
}

export interface AsyncOptions {
  /** A change of it is a `refresh()` (coalesced, never aborts), not a dependency change. */
  refreshKey?: unknown;
}

export const toError = (e: unknown): Error => (e instanceof Error ? e : new Error(String(e)));
export const isAbort = (e: unknown): boolean => e instanceof Error && (e.name === 'AbortError' || (e as { kind?: string }).kind === 'aborted');

/**
 * Coalesces the runs of one loader (pure). At most one run is in flight. Every request made during
 * it is folded into ONE follow-up run that starts after it ends, so the follow-up reads state
 * newer than every request. A refresh never aborts, because the server finishes an aborted query
 * anyway. Abort-and-reissue would only add load, and a loader slower than the requests would
 * never get an answer through.
 */
export class Coalescer {
  private busy = false;
  private dirty = false;

  /**
   * A run is wanted. True means start one now, false means it is folded into the run after the one
   * in flight.
   */
  request(): boolean {
    if (this.busy) {
      this.dirty = true;
      return false;
    }
    this.busy = true;
    return true;
  }

  /** A run starts: it covers every request made so far. */
  started(): void {
    this.busy = true;
    this.dirty = false;
  }

  /** The run in flight ended. True means a request came in during it, so start one more now. */
  settled(): boolean {
    this.busy = false;
    if (!this.dirty) return false;
    this.dirty = false;
    this.busy = true;
    return true;
  }
}

/**
 * Runs `load(signal)` on mount and whenever `deps` change. A dependency change aborts the
 * previous run, and a stale answer is dropped and never shown. `refresh()` and a change of
 * `o.refreshKey` re-run it coalesced instead, with no abort. `data` keeps the last good result
 * while reloading, so tables do not flash empty.
 */
export function useAsync<T>(load: (signal: AbortSignal) => Promise<T>, deps: readonly unknown[], o: AsyncOptions = {}): AsyncState<T> {
  const [state, setState] = useState<{ data: T | undefined; error: Error | null; loading: boolean }>({ data: undefined, error: null, loading: true });
  const [tick, setTick] = useState(0);
  const loadRef = useRef(load);
  loadRef.current = load;
  const runs = useRef<Coalescer | null>(null);
  runs.current ??= new Coalescer();
  useEffect(() => {
    const ctl = new AbortController();
    const c = runs.current!;
    c.started();
    const settle = () => {
      if (c.settled()) setTick((n) => n + 1);
    };
    setState((s) => ({ ...s, loading: true }));
    loadRef.current(ctl.signal).then(
      (data) => {
        if (ctl.signal.aborted) return;
        setState({ data, error: null, loading: false });
        settle();
      },
      (e) => {
        if (ctl.signal.aborted || isAbort(e)) return;
        setState((s) => ({ data: s.data, error: toError(e), loading: false }));
        settle();
      },
    );
    return () => ctl.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, tick]);
  const reload = useCallback(() => setTick((n) => n + 1), []);
  const refresh = useCallback(() => {
    if (runs.current!.request()) setTick((n) => n + 1);
  }, []);
  const firstKey = useRef(true);
  useEffect(() => {
    if (firstKey.current) {
      firstKey.current = false;
      return;
    }
    refresh();
  }, [o.refreshKey]);
  return { ...state, reload, refresh };
}

/**
 * Calls `fn` every `ms` while mounted (and `active`). It does not call it immediately. A hidden
 * page (background tab) pauses it. When the page is shown again, it calls `fn` at once if a
 * period ran out meanwhile, then resumes. `o.background` keeps it running while hidden (the
 * notifications' poll).
 */
export function useInterval(fn: () => void, ms: number, active = true, o: { background?: boolean } = {}): void {
  const ref = useRef(fn);
  ref.current = fn;
  const background = !!o.background;
  useEffect(() => {
    if (!active || ms <= 0) return;
    let last = Date.now();
    let id: ReturnType<typeof setInterval> | null = null;
    const call = () => {
      last = Date.now();
      ref.current();
    };
    const start = () => {
      if (id === null) id = setInterval(call, ms);
    };
    const stop = () => {
      if (id !== null) clearInterval(id);
      id = null;
    };
    if (background || pageVisible()) start();
    const off = background
      ? () => {}
      : onVisibilityChange((visible) => {
          if (!visible) return stop();
          if (Date.now() - last >= ms) call();
          start();
        });
    return () => {
      stop();
      off();
    };
  }, [ms, active, background]);
}

/**
 * The current time in ms, refreshed every `ms` (default 1 s), for "expires in ..." style displays.
 */
export function useNow(ms = 1000): number {
  const [now, setNow] = useState(Date.now());
  useInterval(() => setNow(Date.now()), ms);
  return now;
}

/** The structural part of `document` the visibility helpers read. Tests use a fake. */
export interface VisibilitySource {
  readonly visibilityState: string;
  addEventListener(type: 'visibilitychange', cb: () => void): void;
  removeEventListener(type: 'visibilitychange', cb: () => void): void;
}

const pageDocument = (): VisibilitySource | null => (typeof document === 'undefined' ? null : document);

/**
 * The page is shown: not a background tab and not a minimised window. It is true without a document
 * (tests, workers).
 */
export const pageVisible = (doc: VisibilitySource | null = pageDocument()): boolean => !doc || doc.visibilityState !== 'hidden';

/**
 * Calls `cb(visible)` on every change of the page's visibility and returns the unsubscribe.
 * Background tabs stop polling and live refetches with it, because an open but unseen tab must not
 * keep the indexer busy. They refresh once when shown again.
 */
export function onVisibilityChange(cb: (visible: boolean) => void, doc: VisibilitySource | null = pageDocument()): () => void {
  if (!doc) return () => {};
  const h = () => cb(pageVisible(doc));
  doc.addEventListener('visibilitychange', h);
  return () => doc.removeEventListener('visibilitychange', h);
}

/** Whether the page is shown, re-rendering on every change. */
export function usePageVisible(): boolean {
  const [visible, setVisible] = useState(() => pageVisible());
  useEffect(() => {
    setVisible(pageVisible());
    return onVisibilityChange(setVisible);
  }, []);
  return visible;
}
