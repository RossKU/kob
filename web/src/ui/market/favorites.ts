// The viewer's favourite tokens and pairs. It is a convenience of this browser, with one record
// per network, because covenant ids differ by network. It holds covenant ids only. A ticker is a
// label and never the identity. Nothing here reaches an order, a signature or a server. It is
// pure, but for `useFavorites`.
import { useState } from 'preact/hooks';

export const FAVORITES_KEY_PREFIX = 'kob.favorites.v1:';

export interface FavoritePair { base: string; quote: string }
export interface Favorites { tokens: string[]; pairs: FavoritePair[] }

type Store = Pick<Storage, 'getItem' | 'setItem'>;
const browserStore = (): Store | null => {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
};

const idOf = (v: unknown): string | null => (typeof v === 'string' && /^[0-9a-f]{64}$/i.test(v) ? v.toLowerCase() : null);

/**
 * Validates a stored record. Anything that is not a covenant id or a pair of two different ones is
 * dropped, and so is a repeat, either way round.
 */
export function parseFavorites(v: unknown): Favorites {
  const o = (v && typeof v === 'object' ? v : {}) as { tokens?: unknown; pairs?: unknown };
  const tokens = new Set<string>();
  for (const x of Array.isArray(o.tokens) ? o.tokens : []) {
    const id = idOf(x);
    if (id) tokens.add(id);
  }
  const pairs: FavoritePair[] = [];
  for (const x of Array.isArray(o.pairs) ? o.pairs : []) {
    const base = idOf((x as Partial<FavoritePair> | null)?.base);
    const quote = idOf((x as Partial<FavoritePair> | null)?.quote);
    if (base && quote && base !== quote && !isFavoritePair({ tokens: [], pairs }, base, quote)) pairs.push({ base, quote });
  }
  return { tokens: [...tokens], pairs };
}

/**
 * The favourites of `network`. There are none without storage or with a damaged record. It never
 * throws.
 */
export function readFavorites(network: string, store: Store | null = browserStore()): Favorites {
  try {
    const raw = store?.getItem(FAVORITES_KEY_PREFIX + network);
    return parseFavorites(raw ? JSON.parse(raw) : null);
  } catch {
    return parseFavorites(null);
  }
}

/**
 * Stores the favourites of `network`. It returns false when the browser keeps nothing. It never
 * throws.
 */
export function writeFavorites(network: string, f: Favorites, store: Store | null = browserStore()): boolean {
  try {
    if (!store) return false;
    store.setItem(FAVORITES_KEY_PREFIX + network, JSON.stringify(f));
    return true;
  } catch {
    return false;
  }
}

export const isFavoriteToken = (f: Favorites, id: string): boolean => f.tokens.includes(id.toLowerCase());

/**
 * A pair is one favourite whichever way round it is shown, so the flip of a pair page keeps its
 * star.
 */
export function isFavoritePair(f: Favorites, base: string, quote: string): boolean {
  const [a, b] = [base.toLowerCase(), quote.toLowerCase()];
  return f.pairs.some((p) => (p.base === a && p.quote === b) || (p.base === b && p.quote === a));
}

export function toggleToken(f: Favorites, id: string): Favorites {
  const k = id.toLowerCase();
  return { ...f, tokens: f.tokens.includes(k) ? f.tokens.filter((x) => x !== k) : [...f.tokens, k] };
}

export function togglePair(f: Favorites, base: string, quote: string): Favorites {
  const [a, b] = [base.toLowerCase(), quote.toLowerCase()];
  if (!isFavoritePair(f, a, b)) return { ...f, pairs: [...f.pairs, { base: a, quote: b }] };
  return { ...f, pairs: f.pairs.filter((p) => !((p.base === a && p.quote === b) || (p.base === b && p.quote === a))) };
}

/** What a blocked storage could not keep. The favourites last the visit. */
const visit = new Map<string, Favorites>();

/**
 * The favourites of `network` as component state. A change starts from what is stored, so another
 * tab's change is not lost.
 */
export function useFavorites(network: string): { favorites: Favorites; change: (fn: (f: Favorites) => Favorites) => void } {
  const [favorites, setFavorites] = useState<Favorites>(() => visit.get(network) ?? readFavorites(network));
  const change = (fn: (f: Favorites) => Favorites) => {
    const next = fn(visit.get(network) ?? readFavorites(network));
    if (writeFavorites(network, next)) visit.delete(network);
    else visit.set(network, next);
    setFavorites(next);
  };
  return { favorites, change };
}
