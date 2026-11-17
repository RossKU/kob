import { beforeEach, describe, expect, it } from 'vitest';
import { LOGO_SHOWN_KEY, acquiredTokens, logoGate, readShown, withholdsLogo } from './logo-gate';

const A = 'a'.repeat(64);
const B = 'b'.repeat(64);
const C = 'c'.repeat(64);
const D = 'd'.repeat(64);

const mem = () => {
  const m = new Map<string, string>();
  return { m, getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v) };
};

describe('acquired tokens', () => {
  it('own orders (either side, any status), a pair quote and creation records count', () => {
    const set = acquiredTokens([{ token: A }, { token: B, pair: { quote: C } }], [{ covenantId: D.toUpperCase() }]);
    expect([...set].sort()).toEqual([A, B, C, D]);
  });
  it('unknown or malformed ids are not acquired', () => {
    const set = acquiredTokens([{ token: null }, { token: 'zz' }, { token: A, pair: null }], []);
    expect([...set]).toEqual([A]);
    expect(acquiredTokens([], []).size).toBe(0);
  });
});

describe('logo gate', () => {
  beforeEach(() => logoGate.reset());

  it('a token never acquired nor shown is not allowed; an acquired one is', () => {
    expect(logoGate.allowed(A)).toBe(false);
    logoGate.setAcquired('k', new Set([A]));
    expect(logoGate.allowed(A)).toBe(true);
    expect(logoGate.allowed(B)).toBe(false);
  });

  it('another wallet replaces the set', () => {
    logoGate.setAcquired('k1', new Set([A]));
    logoGate.setAcquired('k2', new Set([B]));
    expect(logoGate.allowed(A)).toBe(false);
    expect(logoGate.allowed(B)).toBe(true);
  });

  it('an unchanged set does not notify; a change does', () => {
    let n = 0;
    const off = logoGate.subscribe(() => n++);
    logoGate.setAcquired('k', new Set([A]));
    logoGate.setAcquired('k', new Set([A]));
    expect(n).toBe(1);
    logoGate.setAcquired('k', new Set([A, B]));
    expect(n).toBe(2);
    off();
  });

  it('"Show logo" is remembered per token, in the page and in storage', () => {
    const st = mem();
    logoGate.show(C, st);
    expect(logoGate.allowed(C)).toBe(true);
    expect(logoGate.allowed(D)).toBe(false);
    expect([...readShown(st)]).toEqual([C]);
    expect(st.m.has(LOGO_SHOWN_KEY)).toBe(true);
  });

  it('a store that throws or holds junk still works', () => {
    const bad = { getItem: () => { throw new Error('x'); }, setItem: () => { throw new Error('x'); } };
    expect(readShown(bad).size).toBe(0);
    logoGate.show(A, bad);
    expect(logoGate.allowed(A)).toBe(true);
    expect(readShown({ getItem: () => '{"a":1}', setItem: () => {} }).size).toBe(0);
    expect([...readShown({ getItem: () => JSON.stringify([A, 5, 'x']), setItem: () => {} })]).toEqual([A]);
  });
});

describe('what a wallet view withholds', () => {
  it('an https logo of a token not acquired, in the wallet scope only', () => {
    expect(withholdsLogo('wallet', false, 'https')).toBe(true);
    expect(withholdsLogo('wallet', true, 'https')).toBe(false);
    expect(withholdsLogo(undefined, false, 'https')).toBe(false);
  });
  it('ipfs:// is never withheld', () => {
    expect(withholdsLogo('wallet', false, 'ipfs')).toBe(false);
  });
});
