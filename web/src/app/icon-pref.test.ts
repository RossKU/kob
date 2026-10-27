import { beforeEach, describe, expect, it } from 'vitest';
import { ICON_PREF_KEY, readIconPref, resetIconPrefMemory, writeIconPref } from './icon-pref';

const store = (initial: Record<string, string> = {}, throws = false) => {
  const m = new Map(Object.entries(initial));
  return {
    m,
    getItem: (k: string) => (throws ? (() => { throw new Error('denied'); })() : (m.get(k) ?? null)),
    setItem: (k: string, v: string) => (throws ? (() => { throw new Error('denied'); })() : void m.set(k, v)),
    removeItem: (k: string) => void m.delete(k),
  };
};

describe('the icon preference', () => {
  beforeEach(() => resetIconPrefMemory());

  it('is on by default, off only when the store says 0', () => {
    expect(readIconPref(store())).toBe(true);
    expect(readIconPref(store({ [ICON_PREF_KEY]: '0' }))).toBe(false);
    expect(readIconPref(store({ [ICON_PREF_KEY]: '1' }))).toBe(true);
    expect(readIconPref(store({ [ICON_PREF_KEY]: 'junk' }))).toBe(true);
  });

  it('a missing or refusing store reads as the default (on)', () => {
    expect(readIconPref(null)).toBe(true);
    expect(readIconPref(store({}, true))).toBe(true);
  });

  it('writing off stores 0, writing on removes the key; the result says whether it was stored', () => {
    const s = store();
    expect(writeIconPref(false, s)).toBe(true);
    expect(s.m.get(ICON_PREF_KEY)).toBe('0');
    expect(readIconPref(s)).toBe(false);
    expect(writeIconPref(true, s)).toBe(true);
    expect(s.m.has(ICON_PREF_KEY)).toBe(false);
    expect(writeIconPref(false, store({}, true))).toBe(false);
    expect(writeIconPref(false, null)).toBe(false);
  });
});
