// Small hooks shared by the views: abortable async loading, polling, a ticking clock.
import { useCallback, useEffect, useRef, useState } from 'preact/hooks';

export interface AsyncState<T> {
  data: T | undefined;
  error: Error | null;
  /** a request is in flight (also true during a reload, while `data` still holds the previous result) */
  loading: boolean;
  /** re-runs the loader now */
  reload(): void;
}

export const toError = (e: unknown): Error => (e instanceof Error ? e : new Error(String(e)));
export const isAbort = (e: unknown): boolean => e instanceof Error && (e.name === 'AbortError' || (e as { kind?: string }).kind === 'aborted');

/**
 * Runs `load(signal)` on mount and whenever `deps` change; a newer run aborts the previous one, and a stale answer is dropped (never shown).
 * `data` keeps the last good result while reloading so tables do not flash empty.
 */
export function useAsync<T>(load: (signal: AbortSignal) => Promise<T>, deps: readonly unknown[]): AsyncState<T> {
  const [state, setState] = useState<{ data: T | undefined; error: Error | null; loading: boolean }>({ data: undefined, error: null, loading: true });
  const [tick, setTick] = useState(0);
  const loadRef = useRef(load);
  loadRef.current = load;
  useEffect(() => {
    const ctl = new AbortController();
    setState((s) => ({ ...s, loading: true }));
    loadRef.current(ctl.signal).then(
      (data) => {
        if (!ctl.signal.aborted) setState({ data, error: null, loading: false });
      },
      (e) => {
        if (!ctl.signal.aborted && !isAbort(e)) setState((s) => ({ data: s.data, error: toError(e), loading: false }));
      },
    );
    return () => ctl.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, tick]);
  const reload = useCallback(() => setTick((n) => n + 1), []);
  return { ...state, reload };
}

/** Calls `fn` every `ms` while mounted (and `active`); does not call it immediately. */
export function useInterval(fn: () => void, ms: number, active = true): void {
  const ref = useRef(fn);
  ref.current = fn;
  useEffect(() => {
    if (!active || ms <= 0) return;
    const id = setInterval(() => ref.current(), ms);
    return () => clearInterval(id);
  }, [ms, active]);
}

/** Current time in ms, refreshed every `ms` (default 1 s): for "expires in ..." style displays. */
export function useNow(ms = 1000): number {
  const [now, setNow] = useState(Date.now());
  useInterval(() => setNow(Date.now()), ms);
  return now;
}
