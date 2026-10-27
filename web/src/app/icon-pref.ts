// The viewer's choice to load token icons from third-party hosts (Settings: "Load token icons from third-party hosts"). On by default. Off: the market
// list shows letter avatars and makes no request to any image host (nothing is fetched from kascov.io's saved copies or a creator's host either).
// A per-browser convenience, not authority: kept in localStorage (`kob.icons` = `0` when off) and the page works the same without storage.
import { useEffect, useState } from 'preact/hooks';

export const ICON_PREF_KEY = 'kob.icons';

type Store = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
const browserStore = (): Store | null => {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
};

/** Whether icons may be loaded: true unless the viewer turned them off. An unreadable store reads as the default (on). */
export function readIconPref(store: Store | null = browserStore()): boolean {
  try {
    return store?.getItem(ICON_PREF_KEY) !== '0';
  } catch {
    return true;
  }
}

const listeners = new Set<(on: boolean) => void>();
/** Stores the choice and tells every open list; false when the browser refused the write (the choice then lasts until the page is closed). */
export function writeIconPref(on: boolean, store: Store | null = browserStore()): boolean {
  // the in-page value first, so a store that refuses writes still turns icons off for this visit
  memory = on;
  for (const l of listeners) l(on);
  try {
    if (!store) return false;
    if (on) store.removeItem(ICON_PREF_KEY);
    else store.setItem(ICON_PREF_KEY, '0');
    return true;
  } catch {
    return false;
  }
}

let memory: boolean | null = null;
const current = (): boolean => memory ?? readIconPref();

/** `[on, set]`: the choice, kept in step across the Settings page and the list. */
export function useIconPref(): [boolean, (on: boolean) => void] {
  const [on, setOn] = useState(current);
  useEffect(() => {
    listeners.add(setOn);
    setOn(current());
    return () => void listeners.delete(setOn);
  }, []);
  return [on, (v) => void writeIconPref(v)];
}

/** Forgets the in-page value (tests). */
export function resetIconPrefMemory(): void {
  memory = null;
}
