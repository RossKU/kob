// How a new creator lock opens (Settings, "Creator lock"; kob/creator-lock.ts). The default is the
// automatic lock: after its date anyone may send the release, which pays only the creator, and no
// signature is needed. "Manual unlock" (`kob.lock.manual` = `1`) makes new locks the manual kind,
// which the creator's own key signs open. It is kept per browser in localStorage and applies to the
// next creation only: a lock on chain keeps its kind. Without storage the page works the same, with
// the default.
import { useEffect, useState } from 'preact/hooks';
import type { LockKind } from '../kob/creator-lock';

/** The localStorage key of the manual-unlock choice (`1` when on). */
export const LOCK_MANUAL_PREF_KEY = 'kob.lock.manual';

type Store = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
const browserStore = (): Store | null => {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
};

const listeners = new Set<(manual: boolean) => void>();
let memory: boolean | null = null;

/** Whether new locks are manual. False (automatic) unless the viewer chose manual. */
export function readLockManualPref(store: Store | null = browserStore()): boolean {
  try {
    return store?.getItem(LOCK_MANUAL_PREF_KEY) === '1';
  } catch {
    return false;
  }
}

const current = (): boolean => memory ?? readLockManualPref();

/** Stores the choice and tells every open view. False when the browser refused the write. */
export function writeLockManualPref(manual: boolean, store: Store | null = browserStore()): boolean {
  memory = manual;
  for (const l of listeners) l(manual);
  try {
    if (!store) return false;
    if (manual) store.setItem(LOCK_MANUAL_PREF_KEY, '1');
    else store.removeItem(LOCK_MANUAL_PREF_KEY);
    return true;
  } catch {
    return false;
  }
}

/** `[manual, set]`, in step across Settings and Create token. */
export function useLockManualPref(): [boolean, (manual: boolean) => void] {
  const [on, setOn] = useState<boolean>(current);
  useEffect(() => {
    listeners.add(setOn);
    setOn(current());
    return () => void listeners.delete(setOn);
  }, []);
  return [on, (v) => void writeLockManualPref(v)];
}

/** The kind of a new lock under the choice. */
export const lockKindOf = (manual: boolean): LockKind => (manual ? 'manual' : 'auto');

/** Forgets the in-page value, for tests. */
export function resetLockPrefMemory(): void {
  memory = null;
}
