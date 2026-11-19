// When the Portfolio offers Unlock for a creator lock (ui/orders/unlock.ts). The node accepts the unlock only once its past median time is past
// the lock's time, and that clock runs behind the wall clock (TN10: 127-130 s). So the button follows the node's past median time, read from the
// app's node while a lock waits to open. A node that does not say (or cannot be reached) leaves the wall clock with a margin.
import { useEffect, useRef, useState } from 'preact/hooks';
import type { NodeApi } from '../../data/node';

/** Without the node's clock: Unlock shows this long after the lock's time by the wall clock (more than TN10's 127-130 s lag). */
export const UNLOCK_MARGIN_MS = 150_000;
/** How often the node's past median time is read while a lock waits to open. */
export const MEDIAN_POLL_MS = 10_000;
/** The longest single wait: the times are checked again at least this often (a computer that slept, a wall clock set forward). */
export const MAX_WAIT_MS = 60_000;

/**
 * Whether the lock is open: by the node's past median time when known (the node takes the unlock once it is past the lock's time), else by the
 * wall clock `now` (ms) with the margin.
 */
export function unlockOpen(until: bigint, median: bigint | null = null, now: number = Date.now()): boolean {
  if (median !== null) return median > until;
  return until + BigInt(UNLOCK_MARGIN_MS) <= BigInt(Math.floor(now));
}

/**
 * How long until the node's clock should be read again (ms), or null when no lock waits. Before the wall clock reaches a lock's time the node's
 * clock cannot be past it either. `canAsk`: the node has the call; `asked`: it was read at least once (a failed read waits a poll, never 0 again).
 */
export function nextUnlockCheck(untils: readonly bigint[], median: bigint | null, now: number, canAsk: boolean, asked: boolean): number | null {
  const waiting = untils.filter((u) => !unlockOpen(u, median, now));
  if (waiting.length === 0) return null;
  const first = Number(waiting.reduce((a, b) => (b < a ? b : a)));
  const wait = !canAsk ? first + UNLOCK_MARGIN_MS - now : now < first ? first - now : asked ? MEDIAN_POLL_MS : 0;
  return Math.min(Math.max(0, wait), MAX_WAIT_MS);
}

/**
 * The node's past median time (unix ms) for the locks with times `untils`, read while one waits to open; null until known. The component
 * renders again when the wall-clock fallback opens a lock.
 */
export function useUnlockClock(node: Pick<NodeApi, 'getPastMedianTime'>, untils: readonly bigint[]): bigint | null {
  const [median, setMedian] = useState<bigint | null>(null);
  const [, setTick] = useState(0);
  const last = useRef<bigint | null>(null);
  const key = [...untils].map(String).sort().join(',');
  useEffect(() => {
    let alive = true;
    let asked = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const canAsk = typeof node.getPastMedianTime === 'function';
    const plan = () => {
      const wait = nextUnlockCheck(untils, last.current, Date.now(), canAsk, asked);
      if (alive && wait !== null) timer = setTimeout(() => void run(), wait);
    };
    const run = async () => {
      // the node is asked only once the wall clock reached a waiting lock's time (before, its clock cannot be past it)
      const due = untils.some((u) => !unlockOpen(u, last.current) && BigInt(Date.now()) >= u);
      if (canAsk && due) {
        asked = true;
        const m = await node.getPastMedianTime!().catch(() => null);
        if (!alive) return;
        // the node's clock never goes back: a failed read keeps the last one
        if (m !== null && (last.current === null || m > last.current)) {
          last.current = m;
          setMedian(m);
        }
      }
      setTick((n) => n + 1);
      plan();
    };
    plan();
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [node, key]);
  return median;
}
