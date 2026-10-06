// "The book is stale" for the market pages: a book that could not be refreshed keeps showing the last good response with a subtle
// "stale since hh:mm:ss" marker instead of disappearing, and only the next good response replaces it.
//
// Stale = the newest pull FAILED, or it has been LATE (still in flight after LATE_MS), or the fresh book is crossed and an older uncrossed snapshot is
// shown (hold-uncrossed.ts). "Since" is always the time of the last good book on screen, never the time of the failure.
import { useEffect, useRef } from 'preact/hooks';
import type { AsyncState } from '../kit';
import { useNow } from '../kit';

/** A pull still in flight after this long is "late". */
export const LATE_MS = 8_000;

export interface FreshnessInput {
  /** when the book on screen was received (null: nothing good yet) */
  lastGoodAt: number | null;
  /** the newest pull failed */
  failed: boolean;
  /** when the pull in flight started (null: none) */
  loadingSince: number | null;
  now: number;
  /** a crossed fresh book is hidden behind the last uncrossed snapshot taken at this time */
  heldAt?: number | null;
  lateMs?: number;
}

/** The time the book on screen dates from when it is stale, else null (pure). Nothing is stale without a book on screen. */
export function staleSinceOf(i: FreshnessInput): number | null {
  const late = i.loadingSince !== null && i.now - i.loadingSince >= (i.lateMs ?? LATE_MS);
  const times: number[] = [];
  if ((i.failed || late) && i.lastGoodAt !== null) times.push(i.lastGoodAt);
  if (i.heldAt !== undefined && i.heldAt !== null) times.push(i.heldAt);
  return times.length ? Math.min(...times) : null;
}

/**
 * Tracks the age of an async resource: `lastGoodAt` moves only when a NEW good result lands (`state.data` is replaced; a failure keeps the old one),
 * `loadingSince` is when the current request started. Returns the `stale since` time (see `staleSinceOf`), re-evaluated every second.
 */
export function useStaleSince(state: AsyncState<unknown>, heldAt: number | null = null): number | null {
  const lastGood = useRef<{ data: unknown; at: number | null }>({ data: undefined, at: null });
  if (state.data !== undefined && state.data !== lastGood.current.data) lastGood.current = { data: state.data, at: Date.now() };
  const started = useRef<number | null>(null);
  if (!state.loading) started.current = null;
  else if (started.current === null) started.current = Date.now();
  const now = useNow(1000);
  // the data of a failed newest pull is the last good one; a failure that lands with no good book yet is the banner's business
  return staleSinceOf({ lastGoodAt: lastGood.current.at, failed: state.error !== null && state.data !== undefined, loadingSince: started.current, now, heldAt });
}

/**
 * `reload` that never aborts a pull in flight: a request that comes in while one is running (a live notice, the poll) is remembered and runs once, right
 * after it, so a slow indexer still gets its answers through (a reload that aborts the previous request would starve the page of any answer).
 */
export function useCoalescedReload(state: Pick<AsyncState<unknown>, 'loading' | 'reload'>): () => void {
  const busy = useRef(state.loading);
  busy.current = state.loading;
  const pending = useRef(false);
  const reload = useRef(state.reload);
  reload.current = state.reload;
  useEffect(() => {
    if (!state.loading && pending.current) {
      pending.current = false;
      reload.current();
    }
  }, [state.loading]);
  return () => {
    if (busy.current) pending.current = true;
    else {
      busy.current = true; // until the loading state lands: a second call in the same tick must not abort this one
      reload.current();
    }
  };
}
