// The tokens the viewer hid from the market list: a convenience of this browser, one record per network (covenant ids differ by network), beside the favourites
// (favorites.ts). A hidden token leaves only the list: its page, its link, the search and the Favorites view still show it, and nothing here reaches an order, a
// signature or a server. Pure, but for `useHiddenTokens`.
import { useState } from 'preact/hooks';

export const HIDDEN_KEY_PREFIX = 'kob.hidden.v1:';

type Store = Pick<Storage, 'getItem' | 'setItem'>;
const browserStore = (): Store | null => {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
};

/** Validates a stored record: covenant ids only (lower case), no repeats; anything else is dropped. */
export function parseHidden(v: unknown): string[] {
  const out = new Set<string>();
  for (const x of Array.isArray(v) ? v : []) if (typeof x === 'string' && /^[0-9a-f]{64}$/i.test(x)) out.add(x.toLowerCase());
  return [...out];
}

/** The hidden tokens of `network` (none without storage or with a damaged record). Never throws. */
export function readHidden(network: string, store: Store | null = browserStore()): string[] {
  try {
    const raw = store?.getItem(HIDDEN_KEY_PREFIX + network);
    return parseHidden(raw ? JSON.parse(raw) : null);
  } catch {
    return [];
  }
}

/** Stores the hidden tokens of `network`. False when the browser keeps nothing. Never throws. */
export function writeHidden(network: string, ids: readonly string[], store: Store | null = browserStore()): boolean {
  try {
    if (!store) return false;
    store.setItem(HIDDEN_KEY_PREFIX + network, JSON.stringify(ids));
    return true;
  } catch {
    return false;
  }
}

export function toggleHidden(ids: readonly string[], id: string): string[] {
  const k = id.toLowerCase();
  return ids.includes(k) ? ids.filter((x) => x !== k) : [...ids, k];
}

/** What a blocked storage could not keep: the hidden tokens last the visit. */
const visit = new Map<string, string[]>();

/** The hidden tokens of `network` as component state. A change starts from what is stored, so another tab's change is not lost. */
export function useHiddenTokens(network: string): { hidden: string[]; toggle: (id: string) => void } {
  const [hidden, setHidden] = useState<string[]>(() => visit.get(network) ?? readHidden(network));
  const toggle = (id: string) => {
    const next = toggleHidden(visit.get(network) ?? readHidden(network), id);
    if (writeHidden(network, next)) visit.delete(network);
    else visit.set(network, next);
    setHidden(next);
  };
  return { hidden, toggle };
}
