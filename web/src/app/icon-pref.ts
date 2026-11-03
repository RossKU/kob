// The viewer's choices about token pictures (Settings, "Token names and icons"), both on by default, both per browser:
//
//   * ICONS (`kob.icons` = `0` when off): the master switch. Off: the market list shows letter avatars and makes no request to any image host (nothing is
//     fetched from kascov.io's saved copies, an IPFS gateway or a creator's host either).
//   * CREATOR SITES (`kob.icons.creator` = `0` when off): a logo address a token's creator gave (https://, or ipfs:// through a public gateway) is loaded
//     from there when kascov.io has no checked copy; that site sees the viewer's IP address and when they looked (data/logos.ts). Off: only kascov.io's
//     checked copies are shown; every other token shows its letter.
//
// Not authority: kept in localStorage and the page works the same without storage.
import { useEffect, useState } from 'preact/hooks';

export const ICON_PREF_KEY = 'kob.icons';
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

/** One on/off preference in localStorage (`key` = `0` when off). */
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
  // the in-page value first, so a store that refuses writes still applies the choice for this visit
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

/** Whether icons may be loaded at all: true unless the viewer turned them off. An unreadable store reads as the default (on). */
export const readIconPref = (store?: Store | null): boolean => icons.read(store);
/** Stores the choice and tells every open list; false when the browser refused the write (the choice then lasts until the page is closed). */
export const writeIconPref = (on: boolean, store?: Store | null): boolean => icons.write(on, store);
/** `[on, set]`: the choice, kept in step across the Settings page and the list. */
export const useIconPref = (): [boolean, (on: boolean) => void] => icons.use();

/** Whether logos may be loaded from the address a creator gave (https:// or ipfs://): true unless the viewer turned it off. */
export const readCreatorImagePref = (store?: Store | null): boolean => creator.read(store);
export const writeCreatorImagePref = (on: boolean, store?: Store | null): boolean => creator.write(on, store);
export const useCreatorImagePref = (): [boolean, (on: boolean) => void] => creator.use();

/** Forgets the in-page values (tests). */
export function resetIconPrefMemory(): void {
  icons.reset();
  creator.reset();
}
