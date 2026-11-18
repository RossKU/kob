// "The book is stale" for the market pages. A book that could not be refreshed keeps showing the
// last good response with a subtle "stale since hh:mm:ss" marker instead of disappearing. Only the
// next good response replaces it.
//
// Stale means the newest pull failed, or it has been late, still in flight after LATE_MS. A
// crossed book is not stale, because it is the live book. "Since" is always the time of the last
// good book on screen, never the time of the failure.
import { useRef } from 'preact/hooks';
import type { AsyncState } from '../kit';
import { useNow } from '../kit';

/** A pull still in flight after this long is "late". */
export const LATE_MS = 8_000;

export interface FreshnessInput {
  /** When the book on screen was received. Null means nothing good yet. */
  lastGoodAt: number | null;
  /** The newest pull failed. */
  failed: boolean;
  /** When the pull in flight started. Null means none. */
  loadingSince: number | null;
  now: number;
  lateMs?: number;
}

/**
 * The time the book on screen dates from when it is stale, else null. It is pure. Nothing is stale
 * without a book on screen.
 */
export function staleSinceOf(i: FreshnessInput): number | null {
  const late = i.loadingSince !== null && i.now - i.loadingSince >= (i.lateMs ?? LATE_MS);
  const times: number[] = [];
  if ((i.failed || late) && i.lastGoodAt !== null) times.push(i.lastGoodAt);
  return times.length ? Math.min(...times) : null;
}

/**
 * Tracks the age of an async resource. `lastGoodAt` moves only when a new good result lands, which
 * is when `state.data` is replaced, and a failure keeps the old one. `loadingSince` is when the
 * current request started. It returns the `stale since` time (see `staleSinceOf`), re-evaluated
 * every second.
 */
export function useStaleSince(state: AsyncState<unknown>): number | null {
  const lastGood = useRef<{ data: unknown; at: number | null }>({ data: undefined, at: null });
  if (state.data !== undefined && state.data !== lastGood.current.data) lastGood.current = { data: state.data, at: Date.now() };
  const started = useRef<number | null>(null);
  if (!state.loading) started.current = null;
  else if (started.current === null) started.current = Date.now();
  const now = useNow(1000);
  // The data of a failed newest pull is the last good one. A failure that lands with no good book
  // yet is the banner's business.
  return staleSinceOf({ lastGoodAt: lastGood.current.at, failed: state.error !== null && state.data !== undefined, loadingSince: started.current, now });
}
