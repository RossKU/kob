import { describe, expect, it } from 'vitest';
import { HIDDEN_KEY_PREFIX, parseHidden, readHidden, toggleHidden, writeHidden } from './hidden-tokens';

const A = 'a'.repeat(64);
const B = 'b'.repeat(64);

const memory = (): Pick<Storage, 'getItem' | 'setItem'> => {
  const m = new Map<string, string>();
  return { getItem: (k) => m.get(k) ?? null, setItem: (k, v) => void m.set(k, v) };
};

describe('hidden tokens', () => {
  it('one record per network, covenant ids only, lower case, no repeats', () => {
    const s = memory();
    expect(writeHidden('testnet-10', [A], s)).toBe(true);
    expect(readHidden('testnet-10', s)).toEqual([A]);
    expect(readHidden('mainnet', s)).toEqual([]);
    expect(parseHidden([A.toUpperCase(), A, 'x', 5, B])).toEqual([A, B]);
    expect(parseHidden({ tokens: [A] })).toEqual([]);
    s.setItem(HIDDEN_KEY_PREFIX + 'mainnet', '{broken');
    expect(readHidden('mainnet', s)).toEqual([]);
  });

  it('hides, then shows again', () => {
    expect(toggleHidden([], A.toUpperCase())).toEqual([A]);
    expect(toggleHidden([A, B], A)).toEqual([B]);
  });

  it('a storage that throws: nothing is kept, nothing throws', () => {
    const blocked = { getItem: () => { throw new Error('SecurityError'); }, setItem: () => { throw new Error('QuotaExceededError'); } };
    expect(readHidden('testnet-10', blocked)).toEqual([]);
    expect(writeHidden('testnet-10', [A], blocked)).toBe(false);
    expect(writeHidden('testnet-10', [A], null)).toBe(false);
  });
});
