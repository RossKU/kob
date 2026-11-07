// The market list's figures as component state: one slot per key (a token's `/v1/stats`, a pair's 24 h volume), fetched a few at a time.
import { useEffect, useRef, useState } from 'preact/hooks';
import { isAbort } from '../kit/hooks';
import { listPageSizeOf, LOADING, NONE, runPool, type ListPageSize, type Slot } from './market-list';

const PAGE_SIZE_KEY = 'kob.market.rows';

/** How long a list or a panel waits for its first figures before it shows what it has (a slow indexer never leaves it empty). */
export const LIST_HOLD_MS = 1500;

/** How long rows take to rise in: `ml-in` in market.css (0.8 s, and 14 rows that wait 34 ms each), rounded up. */
export const RISE_MS = 1300;

/** True once `ready` was true or LIST_HOLD_MS passed since `armed` (a list off screen reads nothing: its time starts when it is shown); it stays true. */
export function useHeldUntil(ready: boolean, armed = true): boolean {
  const [over, setOver] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const timer = setTimeout(() => setOver(true), LIST_HOLD_MS);
    return () => clearTimeout(timer);
  }, [armed]);
  const done = useRef(false);
  if (ready || over) done.current = true;
  return done.current;
}

/** True while the rows `open` let in are rising: a row that comes after that is live and comes as it is. */
export function useRising(open: boolean): boolean {
  const [over, setOver] = useState(false);
  useEffect(() => {
    if (!open) return;
    const timer = setTimeout(() => setOver(true), RISE_MS);
    return () => clearTimeout(timer);
  }, [open]);
  return open && !over;
}

/** The viewer's page size of the list stored under `key`, kept in this browser (a blocked storage keeps it for the visit). */
export function useListPageSize(key: string = PAGE_SIZE_KEY, fallback?: ListPageSize): [ListPageSize, (n: ListPageSize) => void] {
  const [size, setSize] = useState<ListPageSize>(() => {
    try {
      return listPageSizeOf(localStorage.getItem(key), fallback);
    } catch {
      return listPageSizeOf(null, fallback);
    }
  });
  const set = (n: ListPageSize) => {
    setSize(n);
    try {
      localStorage.setItem(key, String(n));
    } catch {
      /* blocked storage */
    }
  };
  return [size, set];
}

/** The page of a list turned to: it belongs to one view of the list (`key`), and any other view, also one seen before, starts at the first page. */
export function useListPage(key: string): [number, (page: number) => void] {
  const [, render] = useState(0);
  const at = useRef({ key, page: 0 });
  if (at.current.key !== key) at.current = { key, page: 0 };
  const set = (page: number) => {
    at.current = { key, page };
    render((n) => n + 1);
  };
  return [at.current.page, set];
}

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
