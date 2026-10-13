// The market list's figures as component state: one slot per key (a token's `/v1/stats`, a pair's 24 h volume), fetched a few at a time.
import { useEffect, useRef, useState } from 'preact/hooks';
import { isAbort } from '../kit/hooks';
import { LOADING, NONE, runPool, type Slot } from './market-list';

/** Calls in flight per list (the indexer rate-limits its heavy routes per client). */
export const LIST_FETCH_LIMIT = 4;

/**
 * One slot per key, filled by `load` (null = unavailable) with at most `LIST_FETCH_LIMIT` calls in flight. `null` keys = not ready to start (the token
 * list is still loading): nothing is fetched. A key that is already there is not fetched again when other keys come and go; `generation` (Refresh)
 * fetches every key again, keeping the figures shown until the new ones arrive. A failed call leaves the key unavailable.
 */
export function usePooledSlots<T>(keys: readonly string[] | null, load: ((key: string, signal: AbortSignal) => Promise<T | null>) | null, generation: number): Map<string, Slot<T>> {
  const [slots, setSlots] = useState<Map<string, Slot<T>>>(() => new Map());
  const current = useRef(slots);
  current.current = slots;
  const loadRef = useRef(load);
  loadRef.current = load;
  const fetchedGen = useRef(generation);
  const joined = keys ? keys.join(',') : null;

  useEffect(() => {
    if (keys === null) return;
    const run = loadRef.current;
    const fresh = fetchedGen.current !== generation;
    fetchedGen.current = generation;
    const have = current.current;
    const todo = run ? keys.filter((k) => fresh || !have.has(k) || have.get(k)!.state === 'loading') : [];
    setSlots((prev) => {
      const next = new Map<string, Slot<T>>();
      for (const k of keys) next.set(k, !run ? NONE : (prev.get(k)?.state === 'ok' ? prev.get(k)! : todo.includes(k) ? LOADING : (prev.get(k) ?? LOADING)));
      return next;
    });
    if (!run || todo.length === 0) return;
    const ctl = new AbortController();
    const put = (k: string, s: Slot<T>) => {
      if (!ctl.signal.aborted) setSlots((prev) => (prev.has(k) ? new Map(prev).set(k, s) : prev));
    };
    void runPool(
      todo,
      LIST_FETCH_LIMIT,
      async (k) => {
        try {
          const v = await run(k, ctl.signal);
          put(k, v === null ? NONE : { state: 'ok', value: v });
        } catch (e) {
          if (!isAbort(e)) put(k, NONE);
        }
      },
      ctl.signal,
    );
    return () => ctl.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [joined, generation, load === null]);

  return slots;
}
