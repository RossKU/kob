// The viewer's choices about token pictures (Settings, "Token names and icons"). Both are on by
// default and kept per browser in localStorage.
//
// - ICONS (`kob.icons`, `0` when off) is the master switch. When off, the market list shows
//   letter avatars and makes no request to any image host. Nothing is fetched from kascov.io's
//   saved copies, an IPFS gateway or a creator's host either.
// - CREATOR SITES (`kob.icons.creator`, `0` when off) allow a logo address that a token's creator
//   gave (https://, or ipfs:// through a public gateway) to load when kascov.io has no checked
//   copy. That site sees the viewer's IP address and when they looked (data/logos.ts). When off,
//   only kascov.io's checked copies are shown, and every other token shows its letter.
//
// The stored values are not authority, and the page works the same without storage.
import { useEffect, useState } from 'preact/hooks';

/** The localStorage key of the icons switch. */
export const ICON_PREF_KEY = 'kob.icons';
/** The localStorage key of the creator-sites switch. */
export const CREATOR_IMAGE_PREF_KEY = 'kob.icons.creator';

type Store = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
const browserStore = (): Store | null => {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
};

interface Pref {
  read(store?: Store | null): boolean;
  write(on: boolean, store?: Store | null): boolean;
  use(): [boolean, (on: boolean) => void];
  reset(): void;
}

/** One on/off preference in localStorage. The key holds `0` when off. */
function makePref(key: string): Pref {
  const listeners = new Set<(on: boolean) => void>();
  let memory: boolean | null = null;
  const read = (store: Store | null = browserStore()): boolean => {
    try {
      return store?.getItem(key) !== '0';
    } catch {
      return true;
    }
  };
  const current = (): boolean => memory ?? read();
  // The in-page value is set first, so a store that refuses writes still applies for this visit.
  const write = (on: boolean, store: Store | null = browserStore()): boolean => {
    memory = on;
    for (const l of listeners) l(on);
    try {
      if (!store) return false;
      if (on) store.removeItem(key);
      else store.setItem(key, '0');
      return true;
    } catch {
      return false;
    }
  };
  const use = (): [boolean, (on: boolean) => void] => {
    const [on, setOn] = useState(current);
    useEffect(() => {
      listeners.add(setOn);
      setOn(current());
      return () => void listeners.delete(setOn);
    }, []);
    return [on, (v) => void write(v)];
  };
  return { read, write, use, reset: () => void (memory = null) };
}

const icons = makePref(ICON_PREF_KEY);
const creator = makePref(CREATOR_IMAGE_PREF_KEY);

/** Whether icons may be loaded at all. True unless the viewer turned them off. */
export const readIconPref = (store?: Store | null): boolean => icons.read(store);
/** Stores the choice and tells every open list. False when the browser refused the write. */
export const writeIconPref = (on: boolean, store?: Store | null): boolean => icons.write(on, store);
/** `[on, set]`. The choice stays in step across the Settings page and the list. */
export const useIconPref = (): [boolean, (on: boolean) => void] => icons.use();

/** Whether logos may load from the address a creator gave (https:// or ipfs://). On by default. */
export const readCreatorImagePref = (store?: Store | null): boolean => creator.read(store);
/** Stores the creator-sites choice and tells every open list. False if the write was refused. */
export const writeCreatorImagePref = (on: boolean, store?: Store | null): boolean => creator.write(on, store);
/** `[on, set]` of the creator-sites choice. */
export const useCreatorImagePref = (): [boolean, (on: boolean) => void] => creator.use();

/** Forgets the in-page values. For tests. */
export function resetIconPrefMemory(): void {
  icons.reset();
  creator.reset();
}
