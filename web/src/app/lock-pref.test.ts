// Settings, "Manual unlock": new creator locks are automatic unless the viewer turns it on.
import { afterEach, describe, expect, it } from 'vitest';
import { LOCK_MANUAL_PREF_KEY, lockKindOf, readLockManualPref, resetLockPrefMemory, writeLockManualPref } from './lock-pref';

const store = () => {
  const m = new Map<string, string>();
  return { m, s: { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), removeItem: (k: string) => void m.delete(k) } };
};

afterEach(() => resetLockPrefMemory());

describe('the manual-unlock choice', () => {
  it('is off by default: new locks open automatically', () => {
    const { s } = store();
    expect(readLockManualPref(s)).toBe(false);
    expect(readLockManualPref(null)).toBe(false);
    expect(lockKindOf(false)).toBe('auto');
  });

  it('is stored as 1 when on and removed when off', () => {
    const { m, s } = store();
    expect(writeLockManualPref(true, s)).toBe(true);
    expect(m.get(LOCK_MANUAL_PREF_KEY)).toBe('1');
    expect(readLockManualPref(s)).toBe(true);
    expect(lockKindOf(true)).toBe('manual');
    writeLockManualPref(false, s);
    expect(m.has(LOCK_MANUAL_PREF_KEY)).toBe(false);
  });

  it('a storage that refuses still keeps the choice for the visit', () => {
    const broken = { getItem: () => { throw new Error('blocked'); }, setItem: () => { throw new Error('blocked'); }, removeItem: () => { throw new Error('blocked'); } };
    expect(readLockManualPref(broken)).toBe(false);
    expect(writeLockManualPref(true, broken)).toBe(false);
  });
});
