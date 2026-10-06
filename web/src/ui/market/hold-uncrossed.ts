// A momentarily CROSSED book (best bid >= best ask, seen while the matchers are catching up and a fill is about to happen) is drawn as a last resort only:
// crossed rows overlap and the spread is negative. The view keeps showing the last UNCROSSED snapshot for a short time with a "matching in
// progress" notice, and after that (or with nothing held, e.g. right after loading or flipping) the crossed book itself: the book never vanishes. Pure state machine + a thin Preact hook.
// The market page holds it for as long as the book stays crossed (`HOLD_FOREVER`): a lagging indexer / matcher keeps a book crossed for a long time and the
// book must not vanish meanwhile; the page marks it "stale since" the time of the snapshot (book-stale.ts).
import { useEffect, useRef, useState } from 'preact/hooks';

/** How long the last uncrossed snapshot stays on screen while the book is crossed. */
export const CROSSED_HOLD_MS = 10_000;
/** Hold the last uncrossed snapshot until the book uncrosses (never expires). */
export const HOLD_FOREVER = Number.POSITIVE_INFINITY;

export interface Held<T> { value: T; at: number }

export interface HoldResult<T> {
  /** what to draw: the fresh value, or the held uncrossed one while it is recent (null only while loading) */
  show: T | null;
  /** the fresh value is crossed: show the "matching in progress" notice */
  matching: boolean;
  held: Held<T> | null;
}

export function holdUncrossed<T>(fresh: T | null, crossed: boolean, held: Held<T> | null, now: number, holdMs = CROSSED_HOLD_MS): HoldResult<T> {
  if (fresh === null) return { show: null, matching: false, held };
  if (!crossed) return { show: fresh, matching: false, held: { value: fresh, at: now } };
  const recent = held !== null && now - held.at < holdMs;
  return { show: recent ? held.value : fresh, matching: true, held };
}

/**
 * Hook form: re-renders when the hold of a still-crossed book expires (no new data may arrive to trigger it). `since` = when the snapshot on screen was taken
 * while the fresh book is crossed (null otherwise). `resetKey`: another subject (orientation, grouping) never shows the snapshot of the previous one.
 */
export function useHeldUncrossed<T>(fresh: T | null, crossed: boolean, holdMs = CROSSED_HOLD_MS, resetKey: unknown = null): { value: T | null; matching: boolean; since: number | null } {
  const held = useRef<Held<T> | null>(null);
  const keyRef = useRef(resetKey);
  if (keyRef.current !== resetKey) {
    keyRef.current = resetKey;
    held.current = null;
  }
  const [, bump] = useState(0);
  const r = holdUncrossed(fresh, crossed, held.current, Date.now(), holdMs);
  held.current = r.held;
  const waitMs = r.matching && r.show !== null && r.held && Number.isFinite(holdMs) ? Math.max(0, r.held.at + holdMs - Date.now()) : null;
  useEffect(() => {
    if (waitMs === null) return;
    const id = setTimeout(() => bump((n) => n + 1), waitMs + 20);
    return () => clearTimeout(id);
  }, [waitMs !== null, fresh]);
  return { value: r.show, matching: r.matching, since: r.matching && r.show !== null && r.held ? r.held.at : null };
}
