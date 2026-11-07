import { describe, expect, it } from 'vitest';
import { FAVORITES_KEY_PREFIX, isFavoritePair, isFavoriteToken, parseFavorites, readFavorites, toggleToken, togglePair, writeFavorites, type Favorites } from './favorites';

const A = 'a'.repeat(64);
const B = 'b'.repeat(64);
const C = 'c'.repeat(64);
const NONE: Favorites = { tokens: [], pairs: [] };

const memory = (): Pick<Storage, 'getItem' | 'setItem'> => {
  const m = new Map<string, string>();
  return { getItem: (k) => m.get(k) ?? null, setItem: (k, v) => void m.set(k, v) };
};
const blocked: Pick<Storage, 'getItem' | 'setItem'> = {
  getItem: () => {
    throw new Error('SecurityError');
  },
  setItem: () => {
    throw new Error('QuotaExceededError');
  },
};

describe('stored favourites', () => {
  it('keeps one record per network', () => {
    const s = memory();
    expect(writeFavorites('testnet-10', { tokens: [A], pairs: [{ base: A, quote: B }] }, s)).toBe(true);
    expect(writeFavorites('mainnet', { tokens: [C], pairs: [] }, s)).toBe(true);
    expect(readFavorites('testnet-10', s)).toEqual({ tokens: [A], pairs: [{ base: A, quote: B }] });
    expect(readFavorites('mainnet', s)).toEqual({ tokens: [C], pairs: [] });
    expect(readFavorites('testnet-12', s)).toEqual(NONE);
  });

  it('a storage that throws, or none at all, keeps nothing and never throws', () => {
    expect(readFavorites('mainnet', blocked)).toEqual(NONE);
    expect(writeFavorites('mainnet', { tokens: [A], pairs: [] }, blocked)).toBe(false);
    expect(readFavorites('mainnet', null)).toEqual(NONE);
    expect(writeFavorites('mainnet', NONE, null)).toBe(false);
  });

  it('a damaged record reads as none', () => {
    for (const raw of ['{', 'null', '7', '"x"', '[]']) {
      const s = memory();
      s.setItem(`${FAVORITES_KEY_PREFIX}mainnet`, raw);
      expect(readFavorites('mainnet', s)).toEqual(NONE);
    }
  });

  it('only covenant ids are kept, in lower case, once each: never a ticker', () => {
    const got = parseFavorites({
      tokens: [A.toUpperCase(), A, 'EXKCC', 7, null, B],
      pairs: [{ base: A, quote: B }, { base: B, quote: A }, { base: A, quote: A }, { base: 'EXKCC', quote: B }, { base: A }, null, 'x'],
    });
    expect(got).toEqual({ tokens: [A, B], pairs: [{ base: A, quote: B }] });
  });
});

describe('toggling', () => {
  it('a token goes in and out', () => {
    const on = toggleToken(NONE, A.toUpperCase());
    expect(on.tokens).toEqual([A]);
    expect(isFavoriteToken(on, A)).toBe(true);
    expect(isFavoriteToken(on, B)).toBe(false);
    expect(toggleToken(on, A).tokens).toEqual([]);
  });

  it('a pair is one favourite either way round: the flip keeps the star and takes it off', () => {
    const on = togglePair(NONE, A, B);
    expect(on.pairs).toEqual([{ base: A, quote: B }]);
    expect(isFavoritePair(on, A, B)).toBe(true);
    expect(isFavoritePair(on, B, A)).toBe(true);
    expect(isFavoritePair(on, A, C)).toBe(false);
    expect(togglePair(on, B, A).pairs).toEqual([]);
  });

  it('a token and a pair of it are separate favourites', () => {
    const f = togglePair(toggleToken(NONE, A), A, B);
    expect(toggleToken(f, A)).toEqual({ tokens: [], pairs: [{ base: A, quote: B }] });
    expect(togglePair(f, A, B)).toEqual({ tokens: [A], pairs: [] });
  });
});
